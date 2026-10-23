use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use llmproxy::{
    management::{ManagementState, oauth::OAuthManager, router},
    providers::{claude::ClaudeAuth, codex::CodexAuth},
    store::PgStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::{Notify, Semaphore};
use tower::ServiceExt;
use uuid::Uuid;

const OWNER_SESSION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FOREIGN_SESSION: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Upstream {
    polls: AtomicUsize,
    exchanges: AtomicUsize,
    reject: AtomicBool,
    malformed: AtomicBool,
    block: AtomicBool,
    started: Notify,
    release: Semaphore,
    bodies: Mutex<Vec<Value>>,
}

impl Default for Upstream {
    fn default() -> Self {
        Self {
            polls: AtomicUsize::new(0),
            exchanges: AtomicUsize::new(0),
            reject: AtomicBool::new(false),
            malformed: AtomicBool::new(false),
            block: AtomicBool::new(false),
            started: Notify::new(),
            release: Semaphore::new(0),
            bodies: Mutex::new(Vec::new()),
        }
    }
}

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    second_pool: PgPool,
    schema: String,
    first: Router,
    second: Router,
    manager: Arc<OAuthManager>,
    other_manager: Arc<OAuthManager>,
    upstream: Arc<Upstream>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let admin = PgPool::connect(&url).await.unwrap();
        let schema = format!("management_oauth_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options = url
            .parse::<sqlx::postgres::PgConnectOptions>()
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options.clone())
            .await
            .unwrap();
        let second_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        let store = PgStore::new(pool.clone());
        store.migrate().await.unwrap();
        let owner = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        for (id, token) in [(owner, OWNER_SESSION), (foreign, FOREIGN_SESSION)] {
            sqlx::query("INSERT INTO accounts(id,label,email,password_hash) VALUES($1,'OAuth fixture',$2,'unused-password-hash')")
                .bind(id).bind(format!("{id}@example.test")).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO management_sessions(id,account_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
                .bind(Uuid::new_v4()).bind(id).bind(Sha256::digest(token.as_bytes()).as_slice()).execute(&pool).await.unwrap();
        }
        let upstream = Arc::new(Upstream::default());
        let poll_state = upstream.clone();
        let exchange_state = upstream.clone();
        let claude_state = upstream.clone();
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(
                &json!({"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-1"}}),
            )
            .unwrap(),
        );
        let app = Router::new()
            .route("/api/accounts/deviceauth/usercode",post(||async {
                Json(json!({"device_auth_id":"device-secret","user_code":"ABCD","interval":"1"}))
            }))
            .route("/api/accounts/deviceauth/token",post(move |Json(body):Json<Value>| {
                let state=poll_state.clone();
                async move {
                    assert_eq!(body["device_auth_id"],"device-secret");
                    assert_eq!(body["user_code"],"ABCD");
                    if state.polls.fetch_add(1,Ordering::SeqCst)==0 {
                        (StatusCode::FORBIDDEN,Json(json!({})))
                    } else {
                        (StatusCode::OK,Json(json!({"authorization_code":"codex-code","code_verifier":"codex-verifier"})))
                    }
                }
            }))
            .route("/oauth/token",post(move |body:String| {
                let state=exchange_state.clone();
                let claims=claims.clone();
                async move {
                    assert!(body.contains("code=codex-code"));
                    assert!(body.contains("code_verifier=codex-verifier"));
                    state.exchanges.fetch_add(1,Ordering::SeqCst);
                    Json(json!({"access_token":"codex-access","refresh_token":"codex-refresh","expires_in":3600,"id_token":format!("header.{claims}.signature")}))
                }
            }))
            .route("/claude/token",post(move |Json(body):Json<Value>| {
                let state=claude_state.clone();
                async move {
                    assert_eq!(body["grant_type"],"authorization_code");
                    assert_eq!(body["redirect_uri"],"https://platform.claude.com/oauth/code/callback");
                    assert_eq!(body["client_id"],"9d1c250a-e61b-44d9-88ed-5944d1962f5e");
                    let number=state.exchanges.fetch_add(1,Ordering::SeqCst)+1;
                    state.bodies.lock().unwrap().push(body);
                    state.started.notify_one();
                    if state.block.load(Ordering::SeqCst) {
                        state.release.acquire().await.unwrap().forget();
                    }
                    if state.reject.load(Ordering::SeqCst) {
                        return (StatusCode::UNAUTHORIZED,"SECRET authorization-code response").into_response();
                    }
                    if state.malformed.load(Ordering::SeqCst) {
                        return Json(json!({"access_token":"SECRET incomplete-token","expires_in":3600,"scope":"user:inference"})).into_response();
                    }
                    Json(json!({"access_token":format!("claude-access-{number}"),"refresh_token":format!("claude-refresh-{number}"),"expires_in":3600,"scope":"user:profile user:inference","refresh_token_expires_in":86400})).into_response()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let manager = Arc::new(OAuthManager::new(
            store.clone(),
            CodexAuth::new(client.clone(), upstream_url.clone()),
            ClaudeAuth::new(client.clone(), format!("{upstream_url}/claude/token")),
        ));
        let second_store = PgStore::new(second_pool.clone());
        let other_manager = Arc::new(OAuthManager::new(
            second_store.clone(),
            CodexAuth::new(client.clone(), upstream_url.clone()),
            ClaudeAuth::new(client, format!("{upstream_url}/claude/token")),
        ));
        let first = router(ManagementState::new(store, manager.clone(), false));
        let second = router(ManagementState::new(
            second_store,
            other_manager.clone(),
            false,
        ));
        Self {
            admin,
            pool,
            second_pool,
            schema,
            first,
            second,
            manager,
            other_manager,
            upstream,
            server,
        }
    }

    async fn provider(&self, kind: &str) -> String {
        let (status, body) = call(
            &self.first,
            "POST",
            "/management/providers",
            Some(OWNER_SESSION),
            json!({"kind":kind,"label":"OAuth provider"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn close(self) {
        self.manager.shutdown().await;
        self.other_manager.shutdown().await;
        self.server.abort();
        self.pool.close().await;
        self.second_pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    session: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("X-LLMProxy-Management", "1");
    if let Some(token) = session {
        request = request.header("cookie", format!("llmproxy_session={token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"body":String::from_utf8_lossy(&bytes)})),
    )
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn browser_logins_are_owned_persistent_single_use_and_refreshable() {
    let f = Fixture::new().await;
    let codex = f.provider("codex").await;
    let claude = f.provider("claude").await;
    let codex_path = format!("/management/providers/{codex}/logins");
    for session in [None, Some("invalid-session")] {
        assert_eq!(
            call(&f.first, "POST", &codex_path, session, json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        call(
            &f.first,
            "POST",
            &codex_path,
            Some(FOREIGN_SESSION),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, login) = call(
        &f.first,
        "POST",
        &codex_path,
        Some(OWNER_SESSION),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{login}");
    assert_eq!(login["user_code"], "ABCD");
    assert!(login.get("device_auth_id").is_none());
    let path = format!("{codex_path}/{}", login["id"].as_str().unwrap());
    assert_eq!(
        call(&f.second, "GET", &path, Some(FOREIGN_SESSION), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&f.second, "GET", &path, Some(OWNER_SESSION), json!({}))
            .await
            .1["status"],
        "pending"
    );
    let complete = format!("{path}/complete");
    assert_eq!(
        call(&f.second, "POST", &complete, Some(OWNER_SESSION), json!({}))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(f.upstream.polls.load(Ordering::SeqCst), 1);
    assert_eq!(
        call(&f.first, "POST", &complete, Some(OWNER_SESSION), json!({}))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        f.upstream.polls.load(Ordering::SeqCst),
        1,
        "early polling contacted upstream"
    );
    sqlx::query("UPDATE provider_login_sessions SET next_poll_at=now()-interval '1 second' WHERE provider_id=$1").bind(&codex).execute(&f.pool).await.unwrap();
    let (status, done) = call(&f.first, "POST", &complete, Some(OWNER_SESSION), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["status"], "completed");
    assert!(!done.to_string().contains("codex-access"));
    assert_eq!(
        call(&f.second, "POST", &complete, Some(OWNER_SESSION), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(f.upstream.exchanges.load(Ordering::SeqCst), 1);
    let stored:(String,i64)=sqlx::query_as("SELECT c.refresh_token,p.login_generation FROM provider_credentials c JOIN provider_instances p ON p.id=c.provider_id WHERE c.provider_id=$1").bind(&codex).fetch_one(&f.pool).await.unwrap();
    assert_eq!(stored, ("codex-refresh".into(), 1));

    let claude_path = format!("/management/providers/{claude}/logins");
    let (_, login) = call(
        &f.first,
        "POST",
        &claude_path,
        Some(OWNER_SESSION),
        json!({}),
    )
    .await;
    let url = reqwest::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.origin().ascii_serialization(), "https://claude.com");
    let query = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        query["redirect_uri"],
        "https://platform.claude.com/oauth/code/callback"
    );
    assert_eq!(query["code_challenge_method"], "S256");
    let verifier: String = sqlx::query_scalar(
        "SELECT code_verifier FROM provider_login_sessions WHERE provider_id=$1",
    )
    .bind(&claude)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        query["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    );
    assert!(!login.to_string().contains(&verifier));
    let path = format!("{claude_path}/{}", login["id"].as_str().unwrap());
    let complete = format!("{path}/complete");
    for code in ["missing-state".to_owned(), "code#wrong-state".to_owned()] {
        assert_eq!(
            call(
                &f.second,
                "POST",
                &complete,
                Some(OWNER_SESSION),
                json!({"code":code})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(f.upstream.exchanges.load(Ordering::SeqCst), 1);
    let code = format!("claude-code#{}", query["state"]);
    assert_eq!(
        call(
            &f.second,
            "POST",
            &complete,
            Some(FOREIGN_SESSION),
            json!({"code":code})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, done) = call(
        &f.second,
        "POST",
        &complete,
        Some(OWNER_SESSION),
        json!({"code":code}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["status"], "completed");
    let payload = f.upstream.bodies.lock().unwrap()[0].clone();
    assert_eq!(payload["code_verifier"], verifier);
    assert_eq!(payload["state"], query["state"].as_ref());
    let stored:(Value,Option<Value>,i64)=sqlx::query_as("SELECT c.credentials,c.oauth_account,p.login_generation FROM claude_credentials c JOIN provider_instances p ON p.id=c.provider_id WHERE c.provider_id=$1").bind(&claude).fetch_one(&f.pool).await.unwrap();
    assert_eq!(
        stored.0["claudeAiOauth"]["refreshToken"],
        "claude-refresh-2"
    );
    assert!(
        stored.0["claudeAiOauth"]["expiresAt"].as_i64().unwrap()
            > chrono::Utc::now().timestamp_millis()
    );
    assert!(
        stored.1.is_none(),
        "optional profile metadata should not gate login"
    );
    assert_eq!(stored.2, 1);
    let cleared: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT state,code_verifier FROM provider_login_sessions WHERE provider_id=$1",
    )
    .bind(&claude)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(cleared, (None, None));
    assert_eq!(
        call(
            &f.first,
            "POST",
            &complete,
            Some(OWNER_SESSION),
            json!({"code":"replay#ignored"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(f.upstream.exchanges.load(Ordering::SeqCst), 2);

    let (_, next) = call(
        &f.first,
        "POST",
        &claude_path,
        Some(OWNER_SESSION),
        json!({}),
    )
    .await;
    assert_eq!(
        call(&f.second, "GET", &path, Some(OWNER_SESSION), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let new_path = format!("{claude_path}/{}/complete", next["id"].as_str().unwrap());
    let url = reqwest::Url::parse(next["authorization_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    f.upstream.reject.store(true, Ordering::SeqCst);
    let (status, error) = call(
        &f.second,
        "POST",
        &new_path,
        Some(OWNER_SESSION),
        json!({"code":format!("rejected-code#{state}")}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert!(!error.to_string().contains("SECRET"));
    assert_eq!(sqlx::query_scalar::<_,String>("SELECT credentials #>> '{claudeAiOauth,refreshToken}' FROM claude_credentials WHERE provider_id=$1").bind(&claude).fetch_one(&f.pool).await.unwrap(),"claude-refresh-2");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT login_generation FROM provider_instances WHERE id=$1")
            .bind(&claude)
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        call(
            &f.first,
            "POST",
            &new_path,
            Some(OWNER_SESSION),
            json!({"code":format!("rejected-code#{state}")})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    f.upstream.reject.store(false, Ordering::SeqCst);
    f.upstream.malformed.store(true, Ordering::SeqCst);
    let (_, malformed) = call(
        &f.first,
        "POST",
        &claude_path,
        Some(OWNER_SESSION),
        json!({}),
    )
    .await;
    let malformed_url =
        reqwest::Url::parse(malformed["authorization_url"].as_str().unwrap()).unwrap();
    let state = malformed_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let (status, error) = call(
        &f.second,
        "POST",
        &format!(
            "{claude_path}/{}/complete",
            malformed["id"].as_str().unwrap()
        ),
        Some(OWNER_SESSION),
        json!({"code":format!("incomplete#{state}")}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!error.to_string().contains("SECRET"));
    assert_eq!(sqlx::query_scalar::<_, String>("SELECT credentials #>> '{claudeAiOauth,refreshToken}' FROM claude_credentials WHERE provider_id=$1").bind(&claude).fetch_one(&f.pool).await.unwrap(), "claude-refresh-2");

    f.upstream.malformed.store(false, Ordering::SeqCst);
    let (_, replacement) = call(
        &f.first,
        "POST",
        &claude_path,
        Some(OWNER_SESSION),
        json!({}),
    )
    .await;
    let replacement_url =
        reqwest::Url::parse(replacement["authorization_url"].as_str().unwrap()).unwrap();
    let state = replacement_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let (status, done) = call(
        &f.second,
        "POST",
        &format!(
            "{claude_path}/{}/complete",
            replacement["id"].as_str().unwrap()
        ),
        Some(OWNER_SESSION),
        json!({"code":format!("replacement#{state}")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let replacement: (String, i64) = sqlx::query_as("SELECT c.credentials #>> '{claudeAiOauth,refreshToken}',p.login_generation FROM claude_credentials c JOIN provider_instances p ON p.id=c.provider_id WHERE c.provider_id=$1")
        .bind(&claude).fetch_one(&f.pool).await.unwrap();
    assert_eq!(replacement, ("claude-refresh-5".into(), 2));

    let (_, expired) = call(
        &f.first,
        "POST",
        &codex_path,
        Some(OWNER_SESSION),
        json!({}),
    )
    .await;
    sqlx::query("UPDATE provider_login_sessions SET expires_at=now()-interval '1 second' WHERE provider_id=$1").bind(&codex).execute(&f.pool).await.unwrap();
    let expired_path = format!("{codex_path}/{}", expired["id"].as_str().unwrap());
    assert_eq!(
        call(
            &f.second,
            "GET",
            &expired_path,
            Some(OWNER_SESSION),
            json!({})
        )
        .await
        .1["status"],
        "expired"
    );
    assert_eq!(
        call(
            &f.second,
            "POST",
            &format!("{expired_path}/complete"),
            Some(OWNER_SESSION),
            json!({})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.upstream.polls.load(Ordering::SeqCst), 2);
    f.close().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn dropped_completion_and_expiry_do_not_discard_an_exchange() {
    let f = Fixture::new().await;
    let provider = f.provider("claude").await;
    let start = format!("/management/providers/{provider}/logins");
    let (_, login) = call(&f.first, "POST", &start, Some(OWNER_SESSION), json!({})).await;
    let login_id = login["id"].as_str().unwrap();
    let status_path = format!("{start}/{login_id}");
    let complete = format!("{status_path}/complete");
    let url = reqwest::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    sqlx::query("UPDATE provider_login_sessions SET expires_at=now()+interval '500 milliseconds' WHERE provider_id=$1").bind(&provider).execute(&f.pool).await.unwrap();
    f.upstream.block.store(true, Ordering::SeqCst);
    let app = f.first.clone();
    let completion = tokio::spawn(async move {
        call(
            &app,
            "POST",
            &complete,
            Some(OWNER_SESSION),
            json!({"code":format!("first#{state}")}),
        )
        .await
    });
    f.upstream.started.notified().await;
    completion.abort();
    assert_eq!(
        call(
            &f.second,
            "POST",
            &format!("{status_path}/complete"),
            Some(OWNER_SESSION),
            json!({"code":"concurrent#unused"})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    tokio::time::sleep(std::time::Duration::from_millis(550)).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        PgStore::new(f.second_pool.clone()).cleanup_expired_sessions(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM provider_login_sessions WHERE provider_id=$1"
        )
        .bind(&provider)
        .fetch_one(&f.pool)
        .await
        .unwrap(),
        1,
        "cleanup discarded an active exchange"
    );
    let app = f.second.clone();
    let start_copy = start.clone();
    let replacement = tokio::spawn(async move {
        call(&app, "POST", &start_copy, Some(OWNER_SESSION), json!({})).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(
        !replacement.is_finished(),
        "replacement discarded an active exchange"
    );
    f.upstream.release.add_permits(1);
    f.manager.shutdown().await;
    let (status, _) = replacement.await.unwrap();
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(f.upstream.exchanges.load(Ordering::SeqCst), 1);
    let saved:(String,i64)=sqlx::query_as("SELECT c.credentials #>> '{claudeAiOauth,refreshToken}',p.login_generation FROM claude_credentials c JOIN provider_instances p ON p.id=c.provider_id WHERE c.provider_id=$1").bind(&provider).fetch_one(&f.pool).await.unwrap();
    assert_eq!(saved, ("claude-refresh-1".into(), 1));
    f.close().await;
}
