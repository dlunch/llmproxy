use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use llmproxy::{
    api::{ApiState, router},
    core::{
        ErrorKind, EventStream, FinishReason, InferenceRequest, Provider, ProviderEvent,
        ProxyError, Role, Tool, ToolChoice, Usage, WebSearchStatus,
    },
    gateway::{Gateway, LeastUsedPolicy},
    metrics::Metrics,
    store::PgStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

struct FakeProvider {
    observed: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for FakeProvider {
    async fn infer(
        &self,
        request: InferenceRequest,
        _: CancellationToken,
    ) -> Result<EventStream, ProxyError> {
        if request
            .messages
            .iter()
            .any(|message| message.content == "setup error")
        {
            return Err(ProxyError::new(
                ErrorKind::InvalidRequest,
                "Unsupported provider option",
            ));
        }
        let tool_call = request
            .messages
            .iter()
            .any(|message| message.content == "call tool")
            && !request
                .messages
                .iter()
                .any(|message| matches!(message.role, Role::Tool));
        let search = request
            .messages
            .last()
            .is_some_and(|message| message.content == "search");
        for message in &request.messages {
            if matches!(message.role, Role::Tool) {
                assert_eq!(message.tool_call_id.as_deref(), Some("call_weather"));
            }
        }
        if tool_call {
            assert!(request.tools.iter().any(|tool| matches!(tool,Tool::Function(function) if function.name == "weather" && function.parameters["type"] == "object")));
            assert!(matches!(
                request.tool_choice,
                ToolChoice::Required | ToolChoice::Function(_)
            ));
        }
        if search {
            assert!(
                request
                    .tools
                    .iter()
                    .any(|tool| matches!(tool, Tool::WebSearch))
            );
        }
        let fail = request
            .messages
            .iter()
            .any(|message| message.content == "fail");
        self.observed.lock().unwrap().push(
            request
                .messages
                .into_iter()
                .map(|message| {
                    format!(
                        "{}:{}",
                        match message.role {
                            Role::System => "system",
                            Role::Developer => "developer",
                            Role::User => "user",
                            Role::Assistant => "assistant",
                            Role::Tool => "tool",
                        },
                        message.content
                    )
                })
                .collect(),
        );
        if tool_call {
            return Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_weather".into()),
                    name: Some("weather".into()),
                    arguments: "{\"city\":".into(),
                }),
                Ok(ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments: "\"Seoul\"}".into(),
                }),
                Ok(ProviderEvent::Finished(FinishReason::ToolCalls)),
            ])));
        }
        if search {
            return Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ProviderEvent::WebSearch {
                    id: "ws_1".into(),
                    query: None,
                    status: WebSearchStatus::InProgress,
                }),
                Ok(ProviderEvent::WebSearch {
                    id: "ws_1".into(),
                    query: Some("weather".into()),
                    status: WebSearchStatus::Searching,
                }),
                Ok(ProviderEvent::WebSearch {
                    id: "ws_1".into(),
                    query: Some("weather".into()),
                    status: WebSearchStatus::Completed,
                }),
                Ok(ProviderEvent::TextDelta("Sunny".into())),
                Ok(ProviderEvent::Citation {
                    url: "https://example.com/weather".into(),
                    title: "Weather".into(),
                    start_index: 0,
                    end_index: 5,
                }),
                Ok(ProviderEvent::Finished(FinishReason::Stop)),
            ])));
        }
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(ProviderEvent::TextDelta("hello ".into())),
            Ok(ProviderEvent::TextDelta("world".into())),
            Ok(ProviderEvent::Usage(Usage {
                input_tokens: 5,
                output_tokens: 2,
            })),
            if fail {
                Err(ProxyError::new(
                    ErrorKind::Upstream,
                    "Controlled provider failure",
                ))
            } else {
                Ok(ProviderEvent::Finished(
                    if request.max_output_tokens == Some(1) {
                        FinishReason::Length
                    } else {
                        FinishReason::Stop
                    },
                ))
            },
        ])))
    }
}

struct Fixture {
    app: Router,
    pool: PgPool,
    admin_pool: PgPool,
    schema: String,
    token: String,
    token_id: Uuid,
    account_id: Uuid,
    observed: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("Set TEST_DATABASE_URL to run PostgreSQL API integration tests");
        let admin_pool = PgPool::connect(&url).await.unwrap();
        let schema = format!("api_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin_pool)
            .await
            .unwrap();
        let search_path = format!("SET search_path TO {schema}");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .after_connect(move |connection, _| {
                let query = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&query).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .unwrap();
        let store = PgStore::new(pool.clone());
        store.migrate().await.unwrap();
        let token = Uuid::new_v4().to_string();
        let token_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts(id,label) VALUES($1,'api-test')")
            .bind(account_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO api_tokens (id,label,token_hash,account_id) VALUES ($1,'api-test',$2,$3)",
        )
        .bind(token_id)
        .bind(Sha256::digest(token.as_bytes()).as_slice())
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
        let provider_id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO provider_instances(id,account_id) VALUES($1,$2)")
            .bind(&provider_id)
            .bind(account_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO provider_models(provider_id,model,upstream_model) VALUES($1,'test','upstream-test')")
            .bind(&provider_id).execute(&pool).await.unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(FakeProvider {
            observed: observed.clone(),
        });
        let metrics = Arc::new(Metrics::default());
        let gateway = Arc::new(Gateway::new(
            store.clone(),
            HashMap::from([(provider_id.clone(), provider)]),
            Arc::new(LeastUsedPolicy),
            metrics.clone(),
            4,
            Duration::from_secs(10),
        ));
        let app = router(ApiState {
            gateway,
            store,
            metrics,
        });
        Self {
            app,
            pool,
            admin_pool,
            schema,
            token,
            token_id,
            account_id,
            observed,
        }
    }

    async fn post(&self, endpoint: &str, body: Value) -> (StatusCode, String) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(endpoint)
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin_pool)
            .await
            .unwrap();
        self.admin_pool.close().await;
    }

    async fn post_from(
        &self,
        peer: Option<&str>,
        authorization: Option<&str>,
        forwarded: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("content-type", "application/json");
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        if let Some(forwarded) = forwarded {
            request = request
                .header("x-forwarded-for", forwarded)
                .header("x-real-ip", forwarded);
        }
        let mut request = request
            .body(Body::from(json!({"model":"test","input":"hi"}).to_string()))
            .unwrap();
        if let Some(peer) = peer {
            request
                .extensions_mut()
                .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        }
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, body)
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn authenticated_protocols_preserve_roles_and_account_usage() {
    let fixture = Fixture::new().await;
    let (status, body) = fixture.post("/v1/chat/completions", json!({"model":"test","messages":[
        {"role":"system","content":"rules"}, {"role":"developer","content":"more rules"},
        {"role":"user","content":[{"type":"text","text":"hi"}]}, {"role":"assistant","content":"hello"},
        {"role":"user","content":"continue"}
    ]})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello world");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["total_tokens"], 7);
    assert_eq!(
        fixture.observed.lock().unwrap()[0],
        [
            "system:rules",
            "developer:more rules",
            "user:hi",
            "assistant:hello",
            "user:continue"
        ]
    );

    let (status, body) = fixture.post("/v1/responses", json!({"model":"test", "instructions":"rules", "input":[
        {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
        {"id":"msg_previous","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"prior","annotations":[]}]},
        {"role":"assistant","content":[{"type":"input_text","text":"more"}]}
    ], "max_output_tokens":1})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["status"], "incomplete");
    assert_eq!(body["incomplete_details"]["reason"], "max_output_tokens");
    assert_eq!(body["output"][0]["status"], "incomplete");
    assert_eq!(body["output"][0]["content"][0]["text"], "hello world");
    assert_eq!(body["usage"]["input_tokens"], 5);
    assert_eq!(
        fixture.observed.lock().unwrap()[1],
        [
            "system:rules",
            "user:hi",
            "assistant:prior",
            "assistant:more"
        ]
    );
    let totals: (i64, i64, i64) = sqlx::query_as("SELECT count(*),sum(input_tokens)::bigint,sum(output_tokens)::bigint FROM requests WHERE api_token_id=$1 AND outcome='succeeded'")
        .bind(fixture.token_id).fetch_one(&fixture.pool).await.unwrap();
    assert_eq!(totals, (2, 10, 4));
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let metrics = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(metrics.contains("llmproxy_requests_total"));
    assert!(metrics.contains("outcome=\"succeeded\"} 2"), "{metrics}");
    assert!(!metrics.contains(&fixture.token));
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn streams_have_stable_ids_ordered_events_and_correct_terminals() {
    let fixture = Fixture::new().await;
    let (status, body) = fixture.post("/v1/chat/completions", json!({"model":"test","messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":true}})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data: Vec<_> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect();
    assert_eq!(data.last(), Some(&"[DONE]"));
    let chunks: Vec<Value> = data[..data.len() - 1]
        .iter()
        .map(|value| serde_json::from_str(value).unwrap())
        .collect();
    assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "hello ");
    assert_eq!(chunks[3]["choices"][0]["finish_reason"], "stop");
    assert_eq!(chunks[4]["usage"]["total_tokens"], 7);
    assert!(chunks.iter().all(|chunk| chunk["id"] == chunks[0]["id"]));

    for length in [false, true] {
        let mut request = json!({"model":"test","input":"hi","stream":true});
        if length {
            request["max_output_tokens"] = json!(1);
        }
        let (status, body) = fixture.post("/v1/responses", request).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let events: Vec<Value> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|value| serde_json::from_str(value).unwrap())
            .collect();
        assert_eq!(events[0]["type"], "response.created");
        assert_eq!(events[1]["type"], "response.in_progress");
        assert_eq!(events[2]["type"], "response.output_item.added");
        let last = events.last().unwrap();
        assert_eq!(
            last["type"],
            if length {
                "response.incomplete"
            } else {
                "response.completed"
            }
        );
        assert_eq!(last["response"]["id"], events[0]["response"]["id"]);
        assert_eq!(last["response"]["output"][0]["id"], events[2]["item"]["id"]);
        assert_eq!(
            last["response"]["output"][0]["content"][0]["text"],
            "hello world"
        );
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event["sequence_number"], index);
        }
    }
    for (endpoint, mut request) in [
        (
            "/v1/chat/completions",
            json!({"model":"test","messages":[{"role":"user","content":"fail"}]}),
        ),
        ("/v1/responses", json!({"model":"test","input":"fail"})),
    ] {
        let (status, body) = fixture.post(endpoint, request.clone()).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        request["stream"] = json!(true);
        let (status, body) = fixture.post(endpoint, request).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("Controlled provider failure"));
        assert!(!body.contains("[DONE]"));
        assert!(!body.contains("response.completed"));
        if endpoint == "/v1/responses" {
            assert!(body.contains("response.failed"));
        }
    }
    for (endpoint, request) in [
        (
            "/v1/chat/completions",
            json!({"model":"test","messages":[{"role":"user","content":"setup error"}],"stream":true}),
        ),
        (
            "/v1/responses",
            json!({"model":"test","input":"setup error","stream":true}),
        ),
    ] {
        let (status, body) = fixture.post(endpoint, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!body.contains("response.created"));
    }
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn invalid_requests_and_revoked_tokens_do_not_reach_providers() {
    let mut fixture = Fixture::new().await;
    for request in [
        json!({"model":"test","messages":[{"role":"tool","content":"hi"}]}),
        json!({"model":"test","messages":[{"role":"user","content":"hi"}],"temperature":3}),
        json!({"model":"test","messages":[{"role":"user","content":"hi"}],"tools":[{"type":"unsupported"}]}),
        json!({"model":"test","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"x"}}]}]}),
        json!({"model":"test","messages":[],"n":2}),
        json!({"model":"test","messages":[{"role":"user","content":"hi"}],"max_tokens":0}),
    ] {
        let (status, body) = fixture.post("/v1/chat/completions", request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    for request in [
        json!({"model":"test","input":"hi","store":true}),
        json!({"model":"test","input":"hi","previous_response_id":"x"}),
    ] {
        assert_eq!(
            fixture.post("/v1/responses", request).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    sqlx::query("UPDATE api_tokens SET revoked_at=now() WHERE id=$1")
        .bind(fixture.token_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let request = json!({"model":"test","input":"hi"});
    assert_eq!(
        fixture.post("/v1/responses", request.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    fixture.token = "invalid".into();
    assert_eq!(
        fixture.post("/v1/responses", request).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(fixture.observed.lock().unwrap().is_empty());
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM requests WHERE api_token_id=$1")
        .bind(fixture.token_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn client_function_calls_round_trip_through_both_protocols() {
    let fixture = Fixture::new().await;
    let function = json!({"name":"weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}});
    let tools = json!([{"type":"function","function":function}]);
    let request = json!({"model":"test","messages":[{"role":"user","content":"call tool"}],"tools":tools,"tool_choice":"required"});
    let (status, body) = fixture.post("/v1/chat/completions", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    let message = &body["choices"][0]["message"];
    assert!(message["content"].is_null());
    assert_eq!(message["tool_calls"][0]["id"], "call_weather");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Seoul\"}"
    );
    let (status, body) = fixture.post("/v1/chat/completions",json!({"model":"test","messages":[
        {"role":"user","content":"call tool"}, message, {"role":"tool","tool_call_id":"call_weather","content":"Sunny"}
    ],"tools":tools})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["choices"][0]["message"]["content"],
        "hello world"
    );
    let mut stream_request = request;
    stream_request["stream"] = json!(true);
    let (status, body) = fixture.post("/v1/chat/completions", stream_request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let chunks: Vec<Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        chunks[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "weather"
    );
    assert_eq!(
        chunks[2]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        "\"Seoul\"}"
    );
    assert_eq!(chunks[3]["choices"][0]["finish_reason"], "tool_calls");

    let mut function = function;
    function["type"] = json!("function");
    let request = json!({"model":"test","input":"call tool","tools":[function],"tool_choice":{"type":"function","name":"weather"}});
    let (status, body) = fixture.post("/v1/responses", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"].as_array().unwrap().len(), 1);
    assert_eq!(body["output"][0]["type"], "function_call");
    assert_eq!(body["output"][0]["call_id"], "call_weather");
    let (status,body) = fixture.post("/v1/responses",json!({"model":"test","tools":[function],"input":[
        {"role":"user","content":"call tool"},body["output"][0],{"type":"function_call_output","call_id":"call_weather","output":"Sunny"}
    ]})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["output"][0]["content"][0]["text"],
        "hello world"
    );
    let mut request = request;
    request["stream"] = json!(true);
    let (status, body) = fixture.post("/v1/responses", request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events: Vec<Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[2]["item"]["type"], "function_call");
    assert_eq!(events[3]["type"], "response.function_call_arguments.delta");
    assert_eq!(events[5]["type"], "response.function_call_arguments.done");
    assert_eq!(events[5]["arguments"], "{\"city\":\"Seoul\"}");
    assert!(!body.contains("response.content_part"));
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn native_web_search_returns_citations_and_responses_search_events() {
    let fixture = Fixture::new().await;
    let (status,body) = fixture.post("/v1/chat/completions",json!({"model":"test","messages":[{"role":"user","content":"search"}],"web_search_options":{}})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Sunny");
    assert_eq!(
        body["choices"][0]["message"]["annotations"][0]["url_citation"]["url"],
        "https://example.com/weather"
    );
    let (status, followup) = fixture.post("/v1/chat/completions",json!({"model":"test","messages":[
        {"role":"user","content":"search"},body["choices"][0]["message"],{"role":"user","content":"continue"}
    ],"web_search_options":{}})).await;
    assert_eq!(status, StatusCode::OK, "{followup}");
    assert_eq!(
        fixture.observed.lock().unwrap().last().unwrap(),
        &["user:search", "assistant:Sunny", "user:continue"]
    );
    let request = json!({"model":"test","input":"search","tools":[{"type":"web_search"}]});
    let (status, body) = fixture.post("/v1/responses", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["output"][0]["type"], "web_search_call");
    assert_eq!(body["output"][0]["action"]["query"], "weather");
    assert_eq!(
        body["output"][1]["content"][0]["annotations"][0]["url"],
        "https://example.com/weather"
    );
    let mut history = vec![json!({"role":"user","content":"search"})];
    history.extend(body["output"].as_array().unwrap().iter().cloned());
    history.push(json!({"role":"user","content":"continue"}));
    let (status, followup) = fixture
        .post(
            "/v1/responses",
            json!({"model":"test","input":history,"tools":[{"type":"web_search"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{followup}");
    assert_eq!(
        fixture.observed.lock().unwrap().last().unwrap(),
        &["user:search", "assistant:Sunny", "user:continue"]
    );
    let mut request = request;
    request["stream"] = json!(true);
    let (status, body) = fixture.post("/v1/responses", request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("response.web_search_call.searching"));
    assert!(body.contains("response.web_search_call.completed"));
    assert!(body.contains("response.output_text.annotation.added"));
    let events: Vec<Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["sequence_number"], index);
    }
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn account_credentials_share_usage_and_ip_auth_uses_only_the_tcp_peer() {
    let fixture = Fixture::new().await;
    let second_token = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO api_tokens(id,account_id,label,token_hash) VALUES($1,$2,'second',$3)")
        .bind(Uuid::new_v4())
        .bind(fixture.account_id)
        .bind(Sha256::digest(second_token.as_bytes()).as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let range_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO account_ip_ranges(id,account_id,network) VALUES($1,$2,'10.20.0.0/16')",
    )
    .bind(range_id)
    .bind(fixture.account_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        fixture
            .post_from(None, Some(&format!("Bearer {}", fixture.token)), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .post_from(None, Some(&format!("Bearer {second_token}")), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, body) = fixture
        .post_from(Some("[::ffff:10.20.2.3]:1234"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let totals: (i64,i64,i64) = sqlx::query_as("SELECT count(*),sum(input_tokens)::bigint,sum(output_tokens)::bigint FROM requests WHERE account_id=$1")
        .bind(fixture.account_id).fetch_one(&fixture.pool).await.unwrap();
    assert_eq!(totals, (3, 15, 6));
    let ip_row: (Option<Uuid>,String) = sqlx::query_as("SELECT api_token_id,host(client_ip) FROM requests WHERE account_id=$1 AND api_token_id IS NULL")
        .bind(fixture.account_id).fetch_one(&fixture.pool).await.unwrap();
    assert_eq!(ip_row, (None, "10.20.2.3".into()));
    for (peer, authorization, forwarded) in [
        (Some("192.0.2.1:1234"), None, Some("10.20.2.3")),
        (None, None, Some("10.20.2.3")),
        (Some("10.20.2.3:1234"), Some("Bearer invalid"), None),
        (Some("10.20.2.3:1234"), Some("Basic ignored"), None),
    ] {
        assert_eq!(
            fixture.post_from(peer, authorization, forwarded).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    sqlx::query("UPDATE accounts SET enabled=false WHERE id=$1")
        .bind(fixture.account_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .post_from(Some("10.20.2.3:1234"), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .post_from(None, Some(&format!("Bearer {}", fixture.token)), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(fixture.observed.lock().unwrap().len(), 3);
    fixture.cleanup().await;
}

#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn json_logs_include_correlated_bodies_and_never_authentication_headers() {
    let fixture = Fixture::new().await;
    let logs = Arc::new(Mutex::new(Vec::new()));
    let writer = LogBuffer(logs.clone());
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // Keep callsite interest stable while other integration tests emit events concurrently.
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let (collected,chat_sse,response_sse) = async {
        let (status,collected) = fixture.post("/v1/chat/completions",json!({"model":"test","messages":[{"role":"user","content":"log input"}]})).await;
        assert_eq!(status,StatusCode::OK,"{collected}");
        let (status,chat_sse) = fixture.post("/v1/chat/completions",json!({"model":"test","messages":[{"role":"user","content":"log stream"}],"stream":true})).await;
        assert_eq!(status,StatusCode::OK,"{chat_sse}");
        let (status,response_sse) = fixture.post("/v1/responses",json!({"model":"test","input":"log stream","stream":true})).await;
        assert_eq!(status,StatusCode::OK,"{response_sse}");
        assert_eq!(fixture.post("/v1/responses",json!({"model":"test","input":"setup error","stream":true})).await.0,StatusCode::BAD_REQUEST);
        assert_eq!(fixture.post("/v1/chat/completions",json!({"model":"test","messages":[{"role":"user","content":"fail"}]})).await.0,StatusCode::BAD_GATEWAY);
        assert_eq!(fixture.post_from(None,Some("Bearer secret-invalid-token"),None).await.0,StatusCode::UNAUTHORIZED);
        (collected,chat_sse,response_sse)
    }.await;
    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains(&fixture.token));
    assert!(!logs.contains("secret-invalid-token"));
    assert!(!logs.to_lowercase().contains("authorization"));
    let events: Vec<Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let requests: Vec<_> = events
        .iter()
        .filter(|event| {
            event["fields"]["event"] == "api_request"
                && event["fields"]["account_id"] == fixture.account_id.to_string()
        })
        .collect();
    assert_eq!(requests.len(), 5, "{logs}");
    for request in &requests {
        assert_eq!(
            request["fields"]["account_id"],
            fixture.account_id.to_string()
        );
        Uuid::parse_str(request["fields"]["request_id"].as_str().unwrap()).unwrap();
    }
    let request: Value =
        serde_json::from_str(requests[0]["fields"]["body_json"].as_str().unwrap()).unwrap();
    assert_eq!(request["messages"][0]["content"], "log input");
    let response = events
        .iter()
        .find(|event| {
            event["fields"]["event"] == "api_response"
                && event["fields"]["account_id"] == fixture.account_id.to_string()
        })
        .unwrap();
    assert_eq!(
        response["fields"]["request_id"],
        requests[0]["fields"]["request_id"]
    );
    assert_eq!(
        response["fields"]["account_id"],
        fixture.account_id.to_string()
    );
    assert_eq!(
        serde_json::from_str::<Value>(response["fields"]["body_json"].as_str().unwrap()).unwrap(),
        serde_json::from_str::<Value>(&collected).unwrap()
    );
    for (index, body) in [(1, chat_sse), (2, response_sse)] {
        let logged: Vec<_> = events
            .iter()
            .filter(|event| {
                event["fields"]["event"] == "api_response_chunk"
                    && event["fields"]["request_id"] == requests[index]["fields"]["request_id"]
            })
            .map(|event| {
                assert_eq!(
                    event["fields"]["account_id"],
                    fixture.account_id.to_string()
                );
                event["fields"]["body_json"].as_str().unwrap()
            })
            .collect();
        let actual: Vec<_> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .collect();
        assert_eq!(logged, actual);
    }
    let error = events
        .iter()
        .find(|event| {
            event["fields"]["event"] == "api_error"
                && event["fields"]["error_kind"] == "unauthorized"
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(error["fields"]["body_json"].as_str().unwrap()).unwrap()["error"]
            ["code"],
        "unauthorized"
    );
    for (index, kind) in [(3, "invalid_request"), (4, "upstream")] {
        let error = events
            .iter()
            .find(|event| {
                event["fields"]["event"] == "api_error"
                    && event["fields"]["error_kind"] == kind
                    && event["fields"]["account_id"] == fixture.account_id.to_string()
            })
            .unwrap();
        assert_eq!(
            error["fields"]["request_id"],
            requests[index]["fields"]["request_id"]
        );
        assert_eq!(
            error["fields"]["account_id"],
            fixture.account_id.to_string()
        );
        serde_json::from_str::<Value>(error["fields"]["body_json"].as_str().unwrap()).unwrap();
    }
    fixture.cleanup().await;
}
