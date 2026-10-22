use axum::{Json, Router, response::IntoResponse, routing::post};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use llmproxy::providers::codex::{CodexAuth, LoginPoll};
use serde_json::{Value, json};

#[tokio::test]
async fn device_login_exchanges_code_and_refreshes_rotated_credentials() {
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "https://api.openai.com/auth": {"chatgpt_account_id":"workspace-1"},
            "exp": 2000000000
        }))
        .unwrap(),
    );
    let id_token = format!("header.{claims}.signature");
    let polls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/api/accounts/deviceauth/usercode", post(|Json(body): Json<Value>| async move {
            assert_eq!(body["client_id"], "app_EMoamEEZ73f0CkXaXp7hrann");
            Json(json!({"device_auth_id":"device-1", "user_code":"ABCD", "interval":"1"}))
        }))
        .route("/api/accounts/deviceauth/token", post(move |Json(body): Json<Value>| {
            let polls = polls.clone();
            async move {
                assert_eq!(body["device_auth_id"], "device-1");
                assert_eq!(body["user_code"], "ABCD");
                if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                    (axum::http::StatusCode::FORBIDDEN, Json(json!({})))
                } else {
                    (axum::http::StatusCode::OK, Json(json!({"authorization_code":"code", "code_verifier":"verifier", "code_challenge":"challenge"})))
                }
            }
        }))
        .route("/oauth/token", post(move |headers: axum::http::HeaderMap, body: String| {
            let id_token = id_token.clone();
            async move {
                if headers["content-type"] == "application/json" {
                    let value: Value = serde_json::from_str(&body).unwrap();
                    assert_eq!(value["grant_type"], "refresh_token");
                    assert_eq!(value["refresh_token"], "refresh-1");
                    Json(json!({"access_token":"access-2", "refresh_token":"refresh-2"}))
                } else {
                    assert!(body.contains("grant_type=authorization_code"));
                    assert!(body.contains("code_verifier=verifier"));
                    Json(json!({"access_token":"access-1", "refresh_token":"refresh-1", "id_token":id_token}))
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let auth = CodexAuth::new(reqwest::Client::new(), url.clone());
    let mut login = auth.start_device_login().await.unwrap();
    assert_eq!(login.verification_url, format!("{url}/codex/device"));
    assert_eq!(login.user_code, "ABCD");
    assert!(matches!(
        auth.poll_device_login(&mut login).await.unwrap(),
        LoginPoll::Pending
    ));
    let poll_started = std::time::Instant::now();
    let LoginPoll::Complete(credentials) = auth.poll_device_login(&mut login).await.unwrap() else {
        panic!("login not complete")
    };
    assert!(poll_started.elapsed() >= std::time::Duration::from_millis(950));
    assert!(matches!(
        auth.poll_device_login(&mut login).await.unwrap(),
        LoginPoll::Expired
    ));
    assert_eq!(credentials.account_id, "workspace-1");
    assert_eq!(credentials.access_token, "access-1");
    let refreshed = auth.refresh(&credentials).await.unwrap();
    assert_eq!(refreshed.access_token, "access-2");
    assert_eq!(refreshed.refresh_token, "refresh-2");
    assert_eq!(refreshed.account_id, "workspace-1");
    server.abort();
}

use futures_util::StreamExt;
use llmproxy::{
    core::{
        ErrorKind, FinishReason, FunctionDefinition, InferenceRequest, Message, Provider,
        ProviderEvent, Role, Tool, ToolCall, ToolChoice, WebSearchStatus,
    },
    providers::codex::CodexProvider,
    store::{CodexCredentials, PgStore},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn request(model: &str) -> InferenceRequest {
    InferenceRequest {
        model: model.into(),
        messages: vec![
            Message {
                role: Role::Developer,
                content: "Be concise".into(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: "Hello".into(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::Assistant,
                content: "Hello back".into(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: "Again".into(),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ],
        max_output_tokens: None,
        temperature: None,
        top_p: None,
        stop: vec![],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn codex_streams_and_refreshes_once_with_persisted_quota_and_failure_outcomes() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let admin = sqlx::PgPool::connect(&database_url).await.unwrap();
    let schema = format!("codex_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = database_url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let store = PgStore::new(pool.clone());
    store.migrate().await.unwrap();
    let provider_id = format!("codex-test-{}", Uuid::new_v4());
    let account_id = Uuid::new_v4();
    sqlx::query("INSERT INTO accounts(id,label) VALUES ($1,'codex fixture')")
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO provider_instances(id,account_id) VALUES ($1,$2)")
        .bind(&provider_id)
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO provider_credentials(provider_id,access_token,refresh_token,account_id,expires_at) VALUES ($1,'expired','refresh-1','workspace-1',now()-interval '1 second') ON CONFLICT(provider_id) DO UPDATE SET access_token='expired',refresh_token='refresh-1',expires_at=now()-interval '1 second'")
        .bind(&provider_id).execute(&pool).await.unwrap();
    let refreshes = Arc::new(AtomicUsize::new(0));
    let counter = refreshes.clone();
    let refresh_started = Arc::new(tokio::sync::Notify::new());
    let refresh_notification = refresh_started.clone();
    let reject_refresh = Arc::new(AtomicBool::new(false));
    let rejection = reject_refresh.clone();
    let omit_expiry = Arc::new(AtomicBool::new(false));
    let missing_expiry = omit_expiry.clone();
    let app = Router::new()
        .route("/oauth/token", post(move |Json(body): Json<Value>| {
            let counter = counter.clone();
            let notification = refresh_notification.clone();
            let rejection = rejection.clone();
            let missing_expiry = missing_expiry.clone();
            async move {
                assert_eq!(body["refresh_token"], "refresh-1");
                counter.fetch_add(1, Ordering::SeqCst);
                notification.notify_waiters();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if rejection.load(Ordering::SeqCst) {
                    return (axum::http::StatusCode::UNAUTHORIZED, "SECRET OAuth response").into_response();
                }
                let mut response = json!({"access_token":"renewed", "refresh_token":"rotated"});
                if !missing_expiry.load(Ordering::SeqCst) {
                    response["expires_in"] = json!(3600);
                }
                Json(response).into_response()
            }
        }))
        .route("/responses", post(|headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
            if matches!(body["model"].as_str(),Some("unauthorized" | "rate-limited")) {
                let status = if body["model"] == "unauthorized" {401} else {429};
                return axum::response::Response::builder().status(status)
                    .body(axum::body::Body::from("SECRET upstream credentials")).unwrap();
            }
            assert_eq!(headers["authorization"], "Bearer renewed");
            assert_eq!(headers["chatgpt-account-id"], "workspace-1");
            assert_eq!(body["stream"], true);
            assert_eq!(body["store"], false);
            assert_eq!(body["input"][0]["role"], "developer");
            assert_eq!(body["input"][2]["role"], "assistant");
            assert_eq!(body["input"][2]["content"][0]["type"], "output_text");
            if body["model"] == "tools" {
                assert_eq!(body["tools"][0]["type"], "web_search");
                assert_eq!(body["tools"][1]["type"], "function");
                assert_eq!(body["tools"][1]["name"], "weather");
                assert_eq!(body["tools"][1]["strict"], true);
                assert_eq!(body["tool_choice"], json!({"type":"function","name":"weather"}));
                assert_eq!(body["input"][4], json!({"type":"function_call","call_id":"previous-call","name":"weather","arguments":"{}"}));
                assert_eq!(body["input"][5], json!({"type":"function_call_output","call_id":"previous-call","output":"sunny"}));
                let call = json!({"type":"function_call","id":"fc-1","call_id":"call-1","name":"weather","arguments":"{\"city\":\"Seoul\"}"});
                let events = [
                    json!({"type":"response.web_search_call.in_progress","item_id":"ws-1"}),
                    json!({"type":"response.web_search_call.searching","item_id":"ws-1"}),
                    json!({"type":"response.output_item.done","item":{"type":"web_search_call","id":"ws-1","status":"completed","action":{"type":"search","query":"Seoul weather"}}}),
                    json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"앞🙂"}),
                    json!({"type":"response.output_text.delta","output_index":2,"content_index":0,"delta":"서울"}),
                    json!({"type":"response.output_text.annotation.added","output_index":2,"content_index":0,"annotation":{"type":"url_citation","url":"https://example.com/weather","title":"Weather","start_index":0,"end_index":2}}),
                    json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"weather","arguments":""}}),
                    json!({"type":"response.function_call_arguments.delta","item_id":"fc-1","delta":"{\"city\":"}),
                    json!({"type":"response.function_call_arguments.delta","item_id":"fc-1","delta":"\"Seoul\"}"}),
                    json!({"type":"response.function_call_arguments.done","item_id":"fc-1","arguments":"{\"city\":\"Seoul\"}"}),
                    json!({"type":"response.output_item.done","item":call}),
                    json!({"type":"response.completed","response":{"output":[call],"usage":{"input_tokens":20,"output_tokens":8}}}),
                ];
                let text = events.iter().map(|event| format!("data: {event}\n\n")).collect::<String>();
                return axum::response::Response::builder().header("content-type","text/event-stream")
                    .body(axum::body::Body::from(text)).unwrap();
            }
            let prefix = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"안녕\"}\r\n\r\n";
            if body["model"] == "cancel" {
                let output = futures_util::stream::iter([Ok::<_, std::convert::Infallible>(prefix.to_owned())])
                    .chain(futures_util::stream::pending());
                return axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from_stream(output)).unwrap();
            }
            let ending = match body["model"].as_str().unwrap() {
                "ok" => "data: {\"type\":\"codex.rate_limits\",\"rate_limits\":{\"primary\":{\"used_percent\":100,\"reset_at\":2000000000},\"secondary\":{\"used_percent\":25,\"reset_at\":2000001000}}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":9,\"output_tokens\":2}}}\n\n".to_string(),
                "eof" => String::new(),
                "bad-json" => "data: {invalid}\n\n".into(),
                "failed" => "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"SECRET\"},\"usage\":{\"input_tokens\":9,\"output_tokens\":1}}}\n\n".into(),
                "incomplete" => "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n".into(),
                "oversize" => format!("data: {}", "x".repeat(1024 * 1024 + 1)),
                "malformed-terminal" => "data: {\"type\":\"response.completed\"}\n\n".into(),
                other => panic!("unknown model {other}"),
            };
            let bytes = format!("{prefix}{ending}").into_bytes();
            let parts = bytes.chunks(7).map(|chunk| Ok::<_, std::convert::Infallible>(chunk.to_vec())).collect::<Vec<_>>();
            axum::response::Response::builder()
                .header("content-type", "text/event-stream")
                .header("x-codex-primary-used-percent", "125")
                .header("x-codex-primary-reset-at", "2000000000")
                .body(axum::body::Body::from_stream(futures_util::stream::iter(parts))).unwrap()
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let metrics = Arc::new(llmproxy::metrics::Metrics::default());
    let provider = Arc::new(CodexProvider::new(
        provider_id.clone(),
        store.clone(),
        client.clone(),
        CodexAuth::new(client.clone(), url.clone()),
        format!("{url}/responses"),
        metrics.clone(),
    ));
    let second_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let second_provider = CodexProvider::new(
        provider_id.clone(),
        PgStore::new(second_pool.clone()),
        client.clone(),
        CodexAuth::new(client, url.clone()),
        format!("{url}/responses"),
        metrics.clone(),
    );
    let mut system_request = request("ok");
    system_request.messages[0].role = Role::System;
    let (a, b) = tokio::join!(
        provider.infer(system_request, CancellationToken::new()),
        second_provider.infer(request("ok"), CancellationToken::new())
    );
    let header_states = store
        .provider_states(account_id, std::slice::from_ref(&provider_id))
        .await
        .unwrap();
    assert!(
        header_states[0]
            .quota
            .iter()
            .any(|window| window.key == "codex_primary" && window.remaining_fraction == 0.0)
    );
    for mut stream in [a.unwrap(), b.unwrap()] {
        assert!(
            matches!(stream.next().await.unwrap().unwrap(), ProviderEvent::TextDelta(text) if text == "안녕")
        );
        assert!(
            matches!(stream.next().await.unwrap().unwrap(), ProviderEvent::Usage(usage) if usage.input_tokens == 9 && usage.output_tokens == 2)
        );
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ProviderEvent::Finished(FinishReason::Stop)
        ));
        assert!(stream.next().await.is_none());
    }
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    let credentials: CodexCredentials = sqlx::query_as("SELECT access_token,refresh_token,account_id,expires_at,updated_at FROM provider_credentials WHERE provider_id=$1")
        .bind(&provider_id).fetch_one(&pool).await.unwrap();
    assert_eq!(credentials.refresh_token, "rotated");
    let before_fresh = metrics.render().unwrap();
    let lock_count = before_fresh.lines().find(|line| {
        line.starts_with("llmproxy_stage_duration_seconds_count{stage=\"credential_lock\"}")
    });
    // Fresh inference must not wait for another replica's credential row lock.
    let (held, _) = store.lock_codex_credentials(&provider_id).await.unwrap();
    let mut unlocked = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        second_provider.infer(request("ok"), CancellationToken::new()),
    )
    .await
    .expect("fresh credentials waited for an exclusive lock")
    .unwrap();
    while let Some(event) = unlocked.next().await {
        event.unwrap();
    }
    held.commit().await.unwrap();
    let after_fresh = metrics.render().unwrap();
    assert_eq!(
        lock_count,
        after_fresh.lines().find(|line| line
            .starts_with("llmproxy_stage_duration_seconds_count{stage=\"credential_lock\"}"))
    );

    provider.refresh_credentials().await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert!(
        store
            .try_lock_codex_credentials("missing-provider")
            .await
            .unwrap()
            .is_none()
    );

    sqlx::query("UPDATE provider_credentials SET refresh_token='refresh-1',expires_at=now()+interval '4 minutes' WHERE provider_id=$1")
        .bind(&provider_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE accounts SET enabled=false WHERE id=$1")
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    provider.refresh_credentials().await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    sqlx::query("UPDATE accounts SET enabled=true WHERE id=$1")
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    let (locked, _) = store.lock_codex_credentials(&provider_id).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        second_provider.refresh_credentials(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    locked.commit().await.unwrap();
    let (first, second) = tokio::join!(
        provider.refresh_credentials(),
        second_provider.refresh_credentials()
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 2);

    sqlx::query("UPDATE provider_credentials SET refresh_token='refresh-1',expires_at=now()+interval '3 days',updated_at=now()-interval '25 hours' WHERE provider_id=$1")
        .bind(&provider_id).execute(&pool).await.unwrap();
    provider.refresh_credentials().await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 3);

    sqlx::query("UPDATE provider_credentials SET refresh_token='refresh-1',expires_at=NULL,updated_at=now()-interval '2 hours' WHERE provider_id=$1")
        .bind(&provider_id).execute(&pool).await.unwrap();
    reject_refresh.store(true, Ordering::SeqCst);
    let error = provider.refresh_credentials().await.unwrap_err();
    assert!(matches!(error.kind, ErrorKind::UpstreamAuth));
    assert!(!error.message.contains("SECRET"));
    let unchanged: CodexCredentials = sqlx::query_as("SELECT access_token,refresh_token,account_id,expires_at,updated_at FROM provider_credentials WHERE provider_id=$1")
        .bind(&provider_id).fetch_one(&pool).await.unwrap();
    assert_eq!(unchanged.refresh_token, "refresh-1");
    assert!(unchanged.updated_at < chrono::Utc::now() - chrono::Duration::hours(1));
    reject_refresh.store(false, Ordering::SeqCst);
    omit_expiry.store(true, Ordering::SeqCst);
    provider.refresh_credentials().await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 5);
    let refreshed: CodexCredentials = sqlx::query_as("SELECT access_token,refresh_token,account_id,expires_at,updated_at FROM provider_credentials WHERE provider_id=$1")
        .bind(&provider_id).fetch_one(&pool).await.unwrap();
    assert_eq!(refreshed.refresh_token, "rotated");
    assert!(refreshed.expires_at.is_none());
    assert!(refreshed.updated_at > unchanged.updated_at);
    provider.refresh_credentials().await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 5);
    omit_expiry.store(false, Ordering::SeqCst);
    let states = store
        .provider_states(account_id, std::slice::from_ref(&provider_id))
        .await
        .unwrap();
    assert_eq!(states[0].quota.len(), 2);
    assert!(
        states[0]
            .quota
            .iter()
            .any(|window| window.key == "codex_primary" && window.remaining_fraction == 0.0)
    );
    assert!(
        states[0]
            .quota
            .iter()
            .any(|window| window.key == "codex_secondary" && window.remaining_fraction == 0.75)
    );
    let mut tool_request = request("tools");
    tool_request.tools = vec![
        Tool::WebSearch,
        Tool::Function(FunctionDefinition {
            name: "weather".into(),
            description: Some("Find weather".into()),
            parameters: json!({"type":"object","properties":{"city":{"type":"string"}}}),
            strict: Some(true),
        }),
    ];
    tool_request.tool_choice = ToolChoice::Function("weather".into());
    tool_request.messages.push(Message {
        role: Role::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "previous-call".into(),
            name: "weather".into(),
            arguments: "{}".into(),
        }],
        tool_call_id: None,
    });
    tool_request.messages.push(Message {
        role: Role::Tool,
        content: "sunny".into(),
        tool_calls: vec![],
        tool_call_id: Some("previous-call".into()),
    });
    let mut stream = provider
        .infer(tool_request, CancellationToken::new())
        .await
        .unwrap();
    let mut arguments = String::new();
    let mut ids = Vec::new();
    let mut search_statuses = Vec::new();
    let mut citations = 0;
    let mut text = String::new();
    let mut tool_finished = false;
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            ProviderEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments: delta,
            } => {
                assert_eq!(index, 0);
                arguments.push_str(&delta);
                if let Some(id) = id {
                    ids.push(id);
                    assert_eq!(name.as_deref(), Some("weather"));
                }
            }
            ProviderEvent::WebSearch { id, query, status } => {
                assert_eq!(id, "ws-1");
                match status {
                    WebSearchStatus::InProgress => search_statuses.push("in_progress"),
                    WebSearchStatus::Searching => search_statuses.push("searching"),
                    WebSearchStatus::Completed => {
                        assert_eq!(query.as_deref(), Some("Seoul weather"));
                        search_statuses.push("completed");
                    }
                }
            }
            ProviderEvent::Citation {
                url,
                title,
                start_index,
                end_index,
            } => {
                assert_eq!(url, "https://example.com/weather");
                assert_eq!(title, "Weather");
                assert_eq!((start_index, end_index), (2, 4));
                citations += 1;
            }
            ProviderEvent::Finished(reason) => {
                assert!(matches!(reason, FinishReason::ToolCalls));
                tool_finished = true;
            }
            ProviderEvent::Usage(usage) => assert_eq!(usage.output_tokens, 8),
            ProviderEvent::TextDelta(delta) => text.push_str(&delta),
        }
    }
    assert_eq!(arguments, "{\"city\":\"Seoul\"}");
    assert_eq!(ids, vec!["call-1"]);
    assert_eq!(text, "앞🙂서울");
    assert_eq!(
        search_statuses,
        vec!["in_progress", "searching", "completed"]
    );
    assert_eq!(citations, 1);
    assert!(tool_finished);
    for model in [
        "eof",
        "bad-json",
        "failed",
        "oversize",
        "malformed-terminal",
        "incomplete",
    ] {
        let mut stream = provider
            .infer(request(model), CancellationToken::new())
            .await
            .unwrap();
        let mut failed = false;
        let mut finished = false;
        let mut partial_usage = false;
        while let Some(event) = stream.next().await {
            match event {
                Err(error) => {
                    assert!(!error.message.contains("SECRET"));
                    failed = true;
                }
                Ok(ProviderEvent::Finished(reason)) => {
                    assert!(matches!(reason, FinishReason::Length));
                    finished = true;
                }
                Ok(ProviderEvent::Usage(usage)) => {
                    assert_eq!(usage.output_tokens, 1);
                    partial_usage = true;
                }
                _ => {}
            }
        }
        assert_eq!(finished, model == "incomplete", "{model}");
        assert_eq!(failed, model != "incomplete", "{model}");
        assert_eq!(partial_usage, model == "failed", "{model}");
    }
    let mut unsupported = request("not-called");
    for model in ["unauthorized", "rate-limited"] {
        let error = match provider
            .infer(request(model), CancellationToken::new())
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("HTTP rejection produced a stream"),
        };
        assert!(!error.message.contains("SECRET"));
        assert!(matches!(
            (&error.kind, model),
            (ErrorKind::UpstreamAuth, "unauthorized") | (ErrorKind::QuotaExceeded, "rate-limited")
        ));
    }
    unsupported.temperature = Some(0.5);
    assert!(
        matches!(provider.infer(unsupported, CancellationToken::new()).await, Err(error) if matches!(error.kind, ErrorKind::InvalidRequest))
    );
    let cancelled = CancellationToken::new();
    let mut stream = provider
        .infer(request("cancel"), cancelled.clone())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ProviderEvent::TextDelta(_)
    ));
    cancelled.cancel();
    assert!(
        matches!(tokio::time::timeout(std::time::Duration::from_secs(1), stream.next()).await.unwrap(), Some(Err(error)) if matches!(error.kind, ErrorKind::Cancelled))
    );
    assert!(
        matches!(provider.infer(request("not-called"), cancelled).await, Err(error) if matches!(error.kind, ErrorKind::Cancelled))
    );
    sqlx::query("INSERT INTO provider_credentials(provider_id,access_token,refresh_token,account_id,expires_at) VALUES ($1,'expired','refresh-1','workspace-1',now()-interval '1 second') ON CONFLICT(provider_id) DO UPDATE SET access_token='expired',refresh_token='refresh-1',expires_at=now()-interval '1 second'")
        .bind(&provider_id).execute(&pool).await.unwrap();
    let cancellation = CancellationToken::new();
    let notification = refresh_started.notified();
    tokio::pin!(notification);
    notification.as_mut().enable();
    let inference = {
        let provider = provider.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move { provider.infer(request("not-called"), cancellation).await })
    };
    notification.await;
    cancellation.cancel();
    assert!(
        matches!(inference.await.unwrap(), Err(error) if matches!(error.kind,ErrorKind::Cancelled))
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), provider.shutdown())
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT refresh_token FROM provider_credentials WHERE provider_id=$1"
        )
        .bind(&provider_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "rotated"
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 6);
    let rendered = metrics.render().unwrap();
    for (stage, expected) in [
        ("credential_refresh", 2),
        ("maintenance_credential_refresh", 4),
    ] {
        let prefix = format!("llmproxy_stage_duration_seconds_count{{stage=\"{stage}\"}} ");
        let observed = rendered
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(
            observed, expected,
            "refresh metrics must include rejection and cancelled callers"
        );
    }
    second_pool.close().await;
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    server.abort();
}
