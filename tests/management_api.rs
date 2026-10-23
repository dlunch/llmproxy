use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use llmproxy::{
    api::{self, ApiState},
    core::{
        ErrorKind, EventStream, FinishReason, InferenceRequest, Provider, ProviderEvent,
        ProviderKind, ProxyError, Usage,
    },
    gateway::{ConfiguredPolicy, Gateway},
    management::{ManagementState, oauth::OAuthManager, router},
    metrics::Metrics,
    providers::{claude::ClaudeAuth, codex::CodexAuth},
    store::PgStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    replica: Router,
    metrics: [Arc<Metrics>; 2],
    pool: PgPool,
    admin: PgPool,
    schema: String,
}

struct FakeProvider;

#[async_trait]
impl Provider for FakeProvider {
    async fn infer(
        &self,
        _: &str,
        request: InferenceRequest,
        _: CancellationToken,
    ) -> Result<EventStream, ProxyError> {
        let fail = request
            .messages
            .iter()
            .any(|message| message.content == "fail");
        Ok(Box::pin(futures_util::stream::iter([
            Ok(ProviderEvent::TextDelta(request.model)),
            Ok(ProviderEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 3,
            })),
            if fail {
                Err(ProxyError::new(
                    ErrorKind::Upstream,
                    "Controlled provider failure",
                ))
            } else {
                Ok(ProviderEvent::Finished(FinishReason::Stop))
            },
        ])))
    }
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("management_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let search_path = format!("SET search_path TO {schema}");
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .unwrap();
        let store = PgStore::new(pool.clone());
        store.migrate().await.unwrap();
        let client = reqwest::Client::new();
        let oauth = Arc::new(OAuthManager::new(
            store.clone(),
            CodexAuth::new(client.clone(), "http://127.0.0.1:1".into()),
            ClaudeAuth::new(client, "http://127.0.0.1:1/token".into()),
        ));
        let metrics = [Arc::new(Metrics::default()), Arc::new(Metrics::default())];
        let [api, replica] = metrics.each_ref().map(|metrics| {
            let provider: Arc<dyn Provider> = Arc::new(FakeProvider);
            let gateway = Arc::new(Gateway::new(
                store.clone(),
                HashMap::from([(ProviderKind::Codex, provider)]),
                Arc::new(ConfiguredPolicy),
                metrics.clone(),
                4,
                Duration::from_secs(10),
            ));
            api::router(ApiState {
                gateway,
                store: store.clone(),
                metrics: metrics.clone(),
            })
        });
        let app = router(ManagementState::new(store, oauth, true)).merge(api);
        Self {
            app,
            replica,
            metrics,
            pool,
            admin,
            schema,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("X-LLMProxy-Management", "1");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, value)
    }

    async fn code(&self) -> String {
        sqlx::query_scalar(include_str!("../scripts/issue-signup-code.sql"))
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn infer(
        &self,
        replica: bool,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let app = if replica { &self.replica } else { &self.app };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn signup(&self, email: &str, code: &str) -> (StatusCode, Value) {
        let (status, _, body) = self.request(Method::POST, "/management/accounts", None,
            json!({"email":email,"password":"a sufficiently long password","label":"Test account","signup_code":code})).await;
        (status, body)
    }

    async fn login(&self, email: &str) -> String {
        let (status, headers, body) = self
            .request(
                Method::POST,
                "/management/sessions",
                None,
                json!({"email":email,"password":"a sufficiently long password"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(headers["cache-control"], "no-store");
        assert!(body.get("token").is_none());
        let cookie = headers["set-cookie"].to_str().unwrap();
        for flag in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/management"] {
            assert!(cookie.contains(flag), "{cookie}");
        }
        cookie.split(';').next().unwrap().to_owned()
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn signup_codes_password_sessions_and_account_isolation() {
    let fixture = Fixture::new().await;
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/management/accounts")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        fixture.signup("one@example.com", "wrong-code").await.0,
        StatusCode::UNAUTHORIZED
    );

    let code = fixture.code().await;
    let (status, account) = fixture.signup(" One@Example.COM ", &code).await;
    assert_eq!(status, StatusCode::CREATED, "{account}");
    assert_eq!(account["email"], "one@example.com");
    assert!(account.get("token").is_none());
    let account_id = Uuid::parse_str(account["id"].as_str().unwrap()).unwrap();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id=$1")
        .bind(account_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert!(!hash.contains("sufficiently"));
    assert_eq!(
        fixture.signup("other@example.com", &code).await.0,
        StatusCode::UNAUTHORIZED
    );

    let reusable = fixture.code().await;
    assert_eq!(
        fixture.signup("one@example.com", &reusable).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        fixture.signup("two@example.com", &reusable).await.0,
        StatusCode::CREATED
    );
    let shared = fixture.code().await;
    let (first, second) = tokio::join!(
        fixture.signup("race1@example.com", &shared),
        fixture.signup("race2@example.com", &shared)
    );
    assert!(
        (first.0 == StatusCode::CREATED && second.0 == StatusCode::UNAUTHORIZED)
            || (second.0 == StatusCode::CREATED && first.0 == StatusCode::UNAUTHORIZED)
    );

    assert_eq!(
        fixture
            .request(
                Method::POST,
                "/management/sessions",
                None,
                json!({"email":"one@example.com","password":"incorrect password"})
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .request(Method::GET, "/management/account", None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let cookie = fixture.login("ONE@example.com").await;
    let other_cookie = fixture.login("two@example.com").await;
    assert_eq!(
        fixture
            .request(
                Method::PATCH,
                "/management/account",
                Some(&cookie),
                json!({"label":"Renamed"})
            )
            .await
            .2["label"],
        "Renamed"
    );

    let mut tokens = Vec::new();
    for label in ["first", "second"] {
        let (status, _, token) = fixture
            .request(
                Method::POST,
                "/management/api-tokens",
                Some(&cookie),
                json!({"label":label}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{token}");
        tokens.push(token);
    }
    let listed = fixture
        .request(
            Method::GET,
            "/management/api-tokens",
            Some(&cookie),
            Value::Null,
        )
        .await
        .2;
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert!(
        !listed
            .to_string()
            .contains(tokens[0]["token"].as_str().unwrap())
    );
    assert_eq!(
        fixture
            .request(
                Method::GET,
                "/management/api-tokens",
                Some(&other_cookie),
                Value::Null
            )
            .await
            .2,
        json!([])
    );
    let secret = tokens[0]["token"].as_str().unwrap();
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/management/account")
                .header("authorization", format!("Bearer {secret}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let token_id = tokens[0]["id"].as_str().unwrap();
    assert_eq!(
        fixture
            .request(
                Method::DELETE,
                &format!("/management/api-tokens/{token_id}"),
                Some(&other_cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .request(
                Method::DELETE,
                &format!("/management/api-tokens/{token_id}"),
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .request(
                Method::DELETE,
                &format!("/management/api-tokens/{token_id}"),
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(
        PgStore::new(fixture.pool.clone())
            .authenticate_token(&Sha256::digest(secret.as_bytes()))
            .await
            .unwrap()
            .is_none()
    );

    assert_eq!(
        fixture
            .request(
                Method::DELETE,
                "/management/session",
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .request(
                Method::GET,
                "/management/account",
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let cookie = fixture.login("one@example.com").await;
    sqlx::query(
        "UPDATE management_sessions SET expires_at=now()-interval '1 second' WHERE account_id=$1",
    )
    .bind(account_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        fixture
            .request(
                Method::GET,
                "/management/account",
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE accounts SET enabled=false WHERE id=$1")
        .bind(account_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .request(
                Method::POST,
                "/management/sessions",
                None,
                json!({"email":"one@example.com","password":"a sufficiently long password"})
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn provider_configuration_atomic_routing_and_per_token_history() {
    let fixture = Fixture::new().await;
    fixture
        .signup("routes@example.com", &fixture.code().await)
        .await;
    fixture
        .signup("foreign@example.com", &fixture.code().await)
        .await;
    let cookie = fixture.login("routes@example.com").await;
    let foreign_cookie = fixture.login("foreign@example.com").await;
    let (status, _, provider) = fixture
        .request(
            Method::POST,
            "/management/providers",
            Some(&cookie),
            json!({"kind":"codex","label":"Main"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{provider}");
    assert_eq!(provider["authenticated"], false);
    let provider_id = provider["id"].as_str().unwrap();
    assert_eq!(
        fixture
            .request(
                Method::GET,
                &format!("/management/providers/{provider_id}"),
                Some(&foreign_cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .request(
                Method::PATCH,
                &format!("/management/providers/{provider_id}"),
                Some(&foreign_cookie),
                json!({"enabled":false})
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .request(
                Method::PATCH,
                &format!("/management/providers/{provider_id}"),
                Some(&cookie),
                json!({"kind":"claude"})
            )
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture
            .request(
                Method::PATCH,
                &format!("/management/providers/{provider_id}"),
                Some(&cookie),
                json!({"enabled":false,"label":"Paused"})
            )
            .await
            .2["enabled"],
        false
    );
    fixture
        .request(
            Method::PATCH,
            &format!("/management/providers/{provider_id}"),
            Some(&cookie),
            json!({"enabled":true}),
        )
        .await;
    let mapping = json!({"strategy":"priority","routes":[{"model":"chat","provider_id":provider_id,"upstream_model":"upstream","priority":2}]});
    assert_eq!(
        fixture
            .request(
                Method::PUT,
                "/management/routing",
                Some(&cookie),
                mapping.clone()
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .request(
                Method::GET,
                "/management/routing",
                Some(&cookie),
                Value::Null
            )
            .await
            .2,
        mapping
    );
    assert_eq!(
        fixture
            .request(
                Method::PUT,
                "/management/routing",
                Some(&foreign_cookie),
                mapping.clone()
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let invalid = json!({"strategy":"least_used","routes":[{"model":"chat","provider_id":"missing","upstream_model":"x","priority":0}]});
    assert_eq!(
        fixture
            .request(Method::PUT, "/management/routing", Some(&cookie), invalid)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .request(
                Method::GET,
                "/management/routing",
                Some(&cookie),
                Value::Null
            )
            .await
            .2,
        mapping
    );
    let duplicate = json!({"strategy":"least_used","routes":[mapping["routes"][0].clone(),mapping["routes"][0].clone()]});
    assert_eq!(
        fixture
            .request(Method::PUT, "/management/routing", Some(&cookie), duplicate)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    let mut tokens = Vec::new();
    for label in ["client one", "client two"] {
        let token = fixture
            .request(
                Method::POST,
                "/management/api-tokens",
                Some(&cookie),
                json!({"label":label}),
            )
            .await
            .2;
        tokens.push(token);
    }
    let first_token = tokens[0]["token"].as_str().unwrap();
    let second_token = tokens[1]["token"].as_str().unwrap();
    let first_id = tokens[0]["id"].as_str().unwrap();
    let second_id = tokens[1]["id"].as_str().unwrap();

    // Both gateways were constructed before any account, provider, or route existed.
    let (status, response) = fixture
        .infer(
            true,
            "/v1/chat/completions",
            first_token,
            json!({"model":"chat","messages":[{"role":"user","content":"hello"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["choices"][0]["message"]["content"], "upstream");
    let (status, response) = fixture
        .infer(
            false,
            "/v1/responses",
            second_token,
            json!({"model":"chat","input":"fail"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{response}");

    let (status, _, secondary) = fixture
        .request(
            Method::POST,
            "/management/providers",
            Some(&cookie),
            json!({"kind":"codex","label":"Secondary"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{secondary}");
    let secondary_id = secondary["id"].as_str().unwrap();
    let mut mapping = json!({"strategy":"priority","routes":[
        {"model":"chat","provider_id":provider_id,"upstream_model":"upstream","priority":2},
        {"model":"chat","provider_id":secondary_id,"upstream_model":"secondary","priority":1}
    ]});
    assert_eq!(
        fixture
            .request(
                Method::PUT,
                "/management/routing",
                Some(&cookie),
                mapping.clone()
            )
            .await
            .0,
        StatusCode::OK
    );
    let (status, response) = fixture
        .infer(
            false,
            "/v1/responses",
            first_token,
            json!({"model":"chat","input":"hello"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["output"][0]["content"][0]["text"], "secondary");

    mapping["strategy"] = json!("least_used");
    mapping["routes"][0]["priority"] = json!(0);
    assert_eq!(
        fixture
            .request(
                Method::PUT,
                "/management/routing",
                Some(&cookie),
                mapping.clone()
            )
            .await
            .0,
        StatusCode::OK
    );
    let (status, response) = fixture
        .infer(
            true,
            "/v1/chat/completions",
            second_token,
            json!({"model":"chat","messages":[{"role":"user","content":"hello"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["choices"][0]["message"]["content"], "secondary");

    mapping["strategy"] = json!("priority");
    mapping["routes"][0]["priority"] = json!(2);
    assert_eq!(
        fixture
            .request(Method::PUT, "/management/routing", Some(&cookie), mapping)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("INSERT INTO provider_quota(provider_id,window_key,observed_at,remaining_fraction,resets_at,login_generation) VALUES($1,'five-hour',now(),0,now()+interval '1 hour',0)")
        .bind(secondary_id).execute(&fixture.pool).await.unwrap();
    let (status, response) = fixture
        .infer(
            false,
            "/v1/responses",
            first_token,
            json!({"model":"chat","input":"hello"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["output"][0]["content"][0]["text"], "upstream");

    assert_eq!(
        fixture
            .request(
                Method::PATCH,
                &format!("/management/providers/{provider_id}"),
                Some(&cookie),
                json!({"enabled":false})
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .infer(
                true,
                "/v1/responses",
                first_token,
                json!({"model":"chat","input":"hello"})
            )
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        fixture
            .request(
                Method::PATCH,
                &format!("/management/providers/{secondary_id}"),
                Some(&cookie),
                json!({"enabled":false})
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .infer(
                false,
                "/v1/responses",
                second_token,
                json!({"model":"chat","input":"hello"})
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .request(
                Method::DELETE,
                &format!("/management/api-tokens/{second_id}"),
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .infer(
                false,
                "/v1/responses",
                second_token,
                json!({"model":"chat","input":"hello"})
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );

    for (replica, provider, token_id, outcome) in [
        (1, provider_id, first_id, "succeeded"),
        (0, provider_id, second_id, "failed"),
        (0, secondary_id, first_id, "succeeded"),
        (1, secondary_id, second_id, "succeeded"),
        (0, provider_id, first_id, "succeeded"),
    ] {
        let metrics = fixture.metrics[replica].render().unwrap();
        assert!(
            metrics
                .lines()
                .any(|line| line.starts_with("llmproxy_requests_total{")
                    && line.contains(&format!("provider=\"{provider}\""))
                    && line.contains(&format!("api_token_id=\"{token_id}\""))
                    && line.contains("auth_source=\"api_token\"")
                    && line.contains(&format!("outcome=\"{outcome}\""))
                    && line.ends_with(" 1")),
            "{metrics}"
        );
    }
    for (replica, token_id, error) in [
        (0, second_id, "model_not_found"),
        (1, first_id, "quota_exceeded"),
    ] {
        let metrics = fixture.metrics[replica].render().unwrap();
        let rejected: Vec<_> = metrics
            .lines()
            .filter(|line| line.starts_with("llmproxy_rejected_requests_total{"))
            .collect();
        assert_eq!(rejected.len(), 1, "{metrics}");
        assert!(
            rejected[0].contains(&format!("api_token_id=\"{token_id}\"")),
            "{metrics}"
        );
        assert!(
            rejected[0].contains(&format!("error_kind=\"{error}\"")),
            "{metrics}"
        );
        assert!(rejected[0].ends_with(" 1"), "{metrics}");
        assert!(!metrics.contains(first_token) && !metrics.contains(second_token));
    }

    let rows: Vec<(String, String, Uuid, String)> = sqlx::query_as(
        "SELECT provider_id,upstream_model,api_token_id,outcome FROM requests ORDER BY started_at",
    )
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    let first_uuid = Uuid::parse_str(first_id).unwrap();
    let second_uuid = Uuid::parse_str(second_id).unwrap();
    assert_eq!(
        rows,
        vec![
            (
                provider_id.into(),
                "upstream".into(),
                first_uuid,
                "succeeded".into()
            ),
            (
                provider_id.into(),
                "upstream".into(),
                second_uuid,
                "failed".into()
            ),
            (
                secondary_id.into(),
                "secondary".into(),
                first_uuid,
                "succeeded".into()
            ),
            (
                secondary_id.into(),
                "secondary".into(),
                second_uuid,
                "succeeded".into()
            ),
            (
                provider_id.into(),
                "upstream".into(),
                first_uuid,
                "succeeded".into()
            ),
        ]
    );
    for (index, token_id) in [first_id, second_id].iter().enumerate() {
        let path = format!("/management/api-tokens/{token_id}/metrics");
        assert_eq!(
            fixture
                .request(Method::GET, &path, Some(&foreign_cookie), Value::Null)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let (status, _, metrics) = fixture
            .request(Method::GET, &path, Some(&cookie), Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK, "{metrics}");
        let count = if index == 0 { 3 } else { 2 };
        assert_eq!(metrics["request_count"], count);
        assert_eq!(metrics["input_tokens"], count * 10);
        assert_eq!(metrics["output_tokens"], count * 3);
        assert_eq!(
            metrics["succeeded_requests"],
            if index == 0 { 3 } else { 1 }
        );
        assert_eq!(metrics["failed_requests"], if index == 1 { 1 } else { 0 });
        assert_eq!(metrics["running_requests"], 0);
        assert_eq!(metrics["missing_usage_requests"], 0);
        assert!(metrics["mean_execution_latency_ms"].as_f64().unwrap() >= 0.0);
        assert!(metrics["last_request_at"].is_string());
    }
    fixture.cleanup().await;
}
