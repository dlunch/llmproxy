use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use futures_util::StreamExt;
use llmproxy::{
    core::{
        EventStream, FinishReason, InferenceRequest, Message, Provider, ProviderEvent, ProxyError,
        Role, Usage,
    },
    gateway::{Gateway, LeastUsedPolicy, Route, RoutingPolicy},
    metrics::Metrics,
    store::{PgStore, QuotaWindow},
};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct FakeProvider;
struct ScopedPolicy {
    owners: HashMap<String, Uuid>,
}

impl RoutingPolicy for ScopedPolicy {
    fn select(
        &self,
        request: &InferenceRequest,
        account: &llmproxy::core::AccountUsage,
        routes: &[Route],
        states: &[llmproxy::store::ProviderState],
    ) -> Result<usize, ProxyError> {
        assert!(
            routes
                .iter()
                .all(|route| self.owners[&route.provider_id] == account.account_id)
        );
        assert!(
            states
                .iter()
                .all(|state| self.owners[&state.provider_id] == account.account_id)
        );
        LeastUsedPolicy.select(request, account, routes, states)
    }
}
#[async_trait]
impl Provider for FakeProvider {
    async fn infer(
        &self,
        request: InferenceRequest,
        _: CancellationToken,
    ) -> Result<EventStream, ProxyError> {
        if request.messages[0].content == "wait" {
            std::future::pending().await
        } else if request.messages[0].content == "oversized-tool" {
            Ok(Box::pin(futures_util::stream::iter([
                Ok(ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("lookup".into()),
                    arguments: " ".repeat(8 * 1024 * 1024),
                }),
                Ok(ProviderEvent::Finished(FinishReason::ToolCalls)),
            ])))
        } else if matches!(
            request.messages[0].content.as_str(),
            "pause" | "usage-then-wait"
        ) {
            let wait = request.messages[0].content == "usage-then-wait";
            Ok(Box::pin(async_stream::stream! {
                yield Ok(ProviderEvent::Usage(Usage { input_tokens: 1, output_tokens: 1 }));
                if wait { std::future::pending::<()>().await; }
                tokio::time::sleep(Duration::from_millis(100)).await;
                yield Ok(ProviderEvent::Finished(FinishReason::Stop));
            }))
        } else if request.messages[0].content == "truncated" {
            Ok(Box::pin(futures_util::stream::iter([Ok(
                ProviderEvent::Usage(Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                }),
            )])))
        } else {
            Ok(Box::pin(futures_util::stream::iter([
                Ok(ProviderEvent::TextDelta(request.model)),
                Ok(ProviderEvent::Usage(Usage {
                    input_tokens: 3,
                    output_tokens: 2,
                })),
                Ok(ProviderEvent::Finished(FinishReason::Stop)),
            ])))
        }
    }
}

fn request(text: &str) -> InferenceRequest {
    InferenceRequest {
        model: "test".into(),
        messages: vec![Message {
            role: Role::User,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }],
        max_output_tokens: None,
        temperature: None,
        top_p: None,
        stop: vec![],
        tools: vec![],
        tool_choice: llmproxy::core::ToolChoice::Auto,
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn postgres_gateway_lifecycle_routing_and_cancellation() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("gateway_{}", Uuid::new_v4().simple());
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
    let account_id = Uuid::new_v4();
    sqlx::query("INSERT INTO accounts(id,label) VALUES ($1,'integration')")
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    let token_id = Uuid::new_v4();
    let principal = || llmproxy::core::Principal {
        account_id,
        credential: llmproxy::core::Credential::ApiToken(token_id),
    };
    let token = Uuid::new_v4().to_string();
    let digest = Sha256::digest(token.as_bytes()).to_vec();
    sqlx::query(
        "INSERT INTO api_tokens (id,label,token_hash,account_id) VALUES ($1,'integration',$2,$3)",
    )
    .bind(token_id)
    .bind(&digest)
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();
    let authenticated = store.authenticate_token(&digest).await.unwrap().unwrap();
    assert_eq!(authenticated.account_id, account_id);
    assert!(
        matches!(authenticated.credential, llmproxy::core::Credential::ApiToken(id) if id == token_id)
    );
    assert!(store.authenticate_token(&[0; 32]).await.unwrap().is_none());
    let prefix = Uuid::new_v4().to_string();
    let a = format!("{prefix}-a");
    let b = format!("{prefix}-b");
    let foreign = format!("{prefix}-0");
    let other_account = Uuid::new_v4();
    sqlx::query("INSERT INTO accounts(id,label) VALUES ($1,'specific')")
        .bind(other_account)
        .execute(&pool)
        .await
        .unwrap();
    for (id, owner) in [
        (&a, account_id),
        (&b, account_id),
        (&foreign, other_account),
    ] {
        sqlx::query("INSERT INTO provider_instances(id,account_id) VALUES ($1,$2)")
            .bind(id)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
    }
    for (provider, model, upstream) in [
        (&a, "test", "a"),
        (&b, "test", "b"),
        (&foreign, "test", "foreign"),
        (&foreign, "foreign-only", "foreign"),
    ] {
        sqlx::query(
            "INSERT INTO provider_models(provider_id,model,upstream_model) VALUES($1,$2,$3)",
        )
        .bind(provider)
        .bind(model)
        .bind(upstream)
        .execute(&pool)
        .await
        .unwrap();
    }
    let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([
        (a.clone(), Arc::new(FakeProvider) as Arc<dyn Provider>),
        (b.clone(), Arc::new(FakeProvider) as Arc<dyn Provider>),
        (foreign.clone(), Arc::new(FakeProvider) as Arc<dyn Provider>),
    ]);
    let metrics = Arc::new(Metrics::default());
    let gateway = Gateway::new(
        store.clone(),
        providers,
        Arc::new(ScopedPolicy {
            owners: HashMap::from([
                (a.clone(), account_id),
                (b.clone(), account_id),
                (foreign.clone(), other_account),
            ]),
        }),
        metrics.clone(),
        8,
        Duration::from_secs(1),
    );
    let mut first = gateway
        .execute(principal(), request("hello"), std::time::Instant::now())
        .await
        .unwrap();
    let id = first.request_id;
    assert!(
        matches!(first.next().await.unwrap().unwrap(), ProviderEvent::TextDelta(text) if text == "a")
    );
    while let Some(event) = first.next().await {
        event.unwrap();
    }
    let row: (String, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT outcome,input_tokens,output_tokens FROM requests WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("succeeded".into(), Some(3), Some(2)));
    let mut second = gateway
        .execute(principal(), request("hello"), std::time::Instant::now())
        .await
        .unwrap();
    assert!(
        matches!(second.next().await.unwrap().unwrap(), ProviderEvent::TextDelta(text) if text == "b")
    );
    while let Some(event) = second.next().await {
        event.unwrap();
    }
    let other_token = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO api_tokens(id,account_id,label,token_hash) VALUES ($1,$2,'foreign',$3)",
    )
    .bind(other_token)
    .bind(other_account)
    .bind(Sha256::digest(other_token.as_bytes()).to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let mut foreign_stream = gateway
        .execute(
            llmproxy::core::Principal {
                account_id: other_account,
                credential: llmproxy::core::Credential::ApiToken(other_token),
            },
            request("hello"),
            std::time::Instant::now(),
        )
        .await
        .unwrap();
    assert!(
        matches!(foreign_stream.next().await.unwrap().unwrap(),ProviderEvent::TextDelta(text) if text=="foreign")
    );
    while let Some(event) = foreign_stream.next().await {
        event.unwrap();
    }
    let mut excluded = request("hello");
    excluded.model = "foreign-only".into();
    assert!(
        matches!(gateway.execute(principal(),excluded, std::time::Instant::now()).await,Err(error) if matches!(error.kind,llmproxy::core::ErrorKind::ModelNotFound))
    );
    assert!(matches!(store.begin_request(llmproxy::store::NewRequest {
        id:Uuid::new_v4(),account_id,token_id:Some(token_id),ip_range_id:None,client_ip:None,
        requested_model:"test",provider_id:&foreign,upstream_model:"test",
        started_at:Utc::now(),deadline_at:Utc::now()+chrono::Duration::seconds(10),
    }).await,Err(error) if matches!(error.kind,llmproxy::core::ErrorKind::Persistence)));
    sqlx::query("DELETE FROM provider_models WHERE provider_id=$1 AND model='foreign-only'")
        .bind(&foreign)
        .execute(&pool)
        .await
        .unwrap();
    let mut removed = request("hello");
    removed.model = "foreign-only".into();
    assert!(matches!(gateway.execute(llmproxy::core::Principal {
        account_id: other_account,
        credential: llmproxy::core::Credential::ApiToken(other_token),
    }, removed, std::time::Instant::now()).await, Err(error) if matches!(error.kind, llmproxy::core::ErrorKind::ModelNotFound)));
    for (id, owner) in [
        (&a, account_id),
        (&b, account_id),
        (&foreign, other_account),
    ] {
        let stats: (Uuid,i64,i64,Option<i64>,Option<i64>,i64,Option<f64>) = sqlx::query_as(
            "SELECT account_id,request_count,succeeded_requests,input_tokens::bigint,output_tokens::bigint,missing_usage_requests,mean_latency_ms FROM provider_statistics WHERE provider_id=$1")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(
            (stats.0, stats.1, stats.2, stats.3, stats.4, stats.5),
            (owner, 1, 1, Some(3), Some(2), 0)
        );
        assert!(stats.6.is_some_and(|latency| latency >= 0.0));
    }
    store
        .save_quota(
            &a,
            &[
                QuotaWindow {
                    key: "short".into(),
                    observed_at: Utc::now(),
                    remaining_fraction: 0.0,
                    resets_at: Some(Utc::now() - chrono::Duration::seconds(1)),
                },
                QuotaWindow {
                    key: "long".into(),
                    observed_at: Utc::now(),
                    remaining_fraction: 0.0,
                    resets_at: None,
                },
            ],
        )
        .await
        .unwrap();
    let mut third = gateway
        .execute(principal(), request("hello"), std::time::Instant::now())
        .await
        .unwrap();
    assert!(
        matches!(third.next().await.unwrap().unwrap(), ProviderEvent::TextDelta(text) if text == "b")
    );
    while let Some(event) = third.next().await {
        event.unwrap();
    }
    let mut timed = gateway
        .execute(principal(), request("wait"), std::time::Instant::now())
        .await
        .unwrap();
    let timed_id = timed.request_id;
    assert!(matches!(
        timed.next().await.unwrap().err().unwrap().kind,
        llmproxy::core::ErrorKind::Timeout
    ));
    let cancelled = gateway
        .execute(principal(), request("wait"), std::time::Instant::now())
        .await
        .unwrap();
    let cancelled_id = cancelled.request_id;
    drop(cancelled);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let outcome: String = sqlx::query_scalar("SELECT outcome FROM requests WHERE id=$1")
                .bind(cancelled_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if outcome == "cancelled" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM requests WHERE id=$1")
        .bind(timed_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(outcome, "timed_out");
    let concurrent = (0..4).map(|_| async {
        let mut stream = gateway
            .execute(principal(), request("hello"), std::time::Instant::now())
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    });
    futures_util::future::join_all(concurrent).await;
    let states = store
        .provider_states(account_id, &[a.clone(), b.clone(), foreign.clone()])
        .await
        .unwrap();
    assert_eq!(states.len(), 2);
    assert_eq!(states.iter().map(|s| s.accumulated_tokens).sum::<u64>(), 35);
    assert_eq!(
        states
            .iter()
            .find(|s| s.provider_id == a)
            .unwrap()
            .quota
            .len(),
        2
    );
    assert!(
        states
            .iter()
            .find(|s| s.provider_id == b)
            .unwrap()
            .quota
            .is_empty()
    );
    let mut unknown = request("hello");
    unknown.model = "unknown".into();
    assert!(matches!(
        gateway
            .execute(principal(), unknown, std::time::Instant::now())
            .await
            .err()
            .unwrap()
            .kind,
        llmproxy::core::ErrorKind::ModelNotFound
    ));
    store
        .save_quota(
            &b,
            &[QuotaWindow {
                key: "long".into(),
                observed_at: Utc::now(),
                remaining_fraction: 0.0,
                resets_at: None,
            }],
        )
        .await
        .unwrap();
    assert!(matches!(
        gateway
            .execute(principal(), request("hello"), std::time::Instant::now())
            .await
            .err()
            .unwrap()
            .kind,
        llmproxy::core::ErrorKind::QuotaExceeded
    ));
    sqlx::query(
        "UPDATE provider_quota SET observed_at=now()-interval '6 minutes' WHERE provider_id=$1",
    )
    .bind(&b)
    .execute(&pool)
    .await
    .unwrap();
    let mut broken = gateway
        .execute(principal(), request("truncated"), std::time::Instant::now())
        .await
        .unwrap();
    let broken_id = broken.request_id;
    assert!(matches!(
        broken.next().await.unwrap().unwrap(),
        ProviderEvent::Usage(_)
    ));
    assert!(matches!(
        broken.next().await.unwrap().err().unwrap().kind,
        llmproxy::core::ErrorKind::Upstream
    ));
    let row: (String, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT outcome,input_tokens,output_tokens FROM requests WHERE id=$1")
            .bind(broken_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("failed".into(), Some(1), Some(1)));
    assert!(matches!(
        gateway
            .execute(
                llmproxy::core::Principal {
                    account_id,
                    credential: llmproxy::core::Credential::ApiToken(Uuid::new_v4())
                },
                request("hello"),
                std::time::Instant::now()
            )
            .await
            .err()
            .unwrap()
            .kind,
        llmproxy::core::ErrorKind::Persistence
    ));
    let mut unsaved = gateway
        .execute(principal(), request("pause"), std::time::Instant::now())
        .await
        .unwrap();
    let unsaved_id = unsaved.request_id;
    assert!(matches!(
        unsaved.next().await.unwrap().unwrap(),
        ProviderEvent::Usage(_)
    ));
    sqlx::query("DELETE FROM requests WHERE id=$1")
        .bind(unsaved_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        unsaved.next().await.unwrap().err().unwrap().kind,
        llmproxy::core::ErrorKind::Persistence
    ));
    let mut oversized = gateway
        .execute(
            principal(),
            request("oversized-tool"),
            std::time::Instant::now(),
        )
        .await
        .unwrap();
    assert!(matches!(
        oversized.next().await.unwrap().err().unwrap().kind,
        llmproxy::core::ErrorKind::Upstream
    ));
    assert!(oversized.next().await.is_none());
    let range_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO account_ip_ranges(id,account_id,network) VALUES ($1,$2,'192.0.2.0/24')",
    )
    .bind(range_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();
    let client_ip: std::net::IpAddr = "192.0.2.10".parse().unwrap();
    let mut trusted = gateway
        .execute(
            store.authenticate_ip(client_ip).await.unwrap().unwrap(),
            request("hello"),
            std::time::Instant::now(),
        )
        .await
        .unwrap();
    let trusted_id = trusted.request_id;
    while let Some(event) = trusted.next().await {
        event.unwrap();
    }
    let identity: (Option<Uuid>, Option<ipnetwork::IpNetwork>) =
        sqlx::query_as("SELECT api_token_id,client_ip FROM requests WHERE id=$1")
            .bind(trusted_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(identity.0.is_none());
    assert_eq!(identity.1.unwrap().ip(), client_ip);
    let usage = store.account_usage(account_id).await.unwrap();
    assert_eq!(usage.account_id, account_id);
    assert_eq!(usage.input_tokens + usage.output_tokens, 42);
    let specific_range = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO account_ip_ranges(id,account_id,network) VALUES ($1,$2,'192.0.2.10/32')",
    )
    .bind(specific_range)
    .bind(other_account)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        store
            .authenticate_ip(client_ip)
            .await
            .unwrap()
            .unwrap()
            .account_id,
        other_account
    );
    sqlx::query("UPDATE accounts SET enabled=false WHERE id=$1")
        .bind(other_account)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_ip(client_ip)
            .await
            .unwrap()
            .unwrap()
            .account_id,
        account_id
    );
    assert!(
        store
            .authenticate_ip("198.51.100.1".parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let second_digest = Sha256::digest(Uuid::new_v4().as_bytes()).to_vec();
    let second_token = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO api_tokens(id,account_id,label,token_hash) VALUES ($1,$2,'second',$3)",
    )
    .bind(second_token)
    .bind(account_id)
    .bind(&second_digest)
    .execute(&pool)
    .await
    .unwrap();
    let mut second_identity = gateway
        .execute(
            store
                .authenticate_token(&second_digest)
                .await
                .unwrap()
                .unwrap(),
            request("hello"),
            std::time::Instant::now(),
        )
        .await
        .unwrap();
    while let Some(event) = second_identity.next().await {
        event.unwrap();
    }
    let usage = store.account_usage(account_id).await.unwrap();
    assert_eq!(usage.input_tokens + usage.output_tokens, 47);
    let mut timed_with_usage = gateway
        .execute(
            principal(),
            request("usage-then-wait"),
            std::time::Instant::now(),
        )
        .await
        .unwrap();
    assert!(matches!(
        timed_with_usage.next().await.unwrap().unwrap(),
        ProviderEvent::Usage(_)
    ));
    assert!(matches!(
        timed_with_usage.next().await.unwrap().err().unwrap().kind,
        llmproxy::core::ErrorKind::Timeout
    ));
    let row: (String, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT outcome,input_tokens,output_tokens FROM requests WHERE id=$1")
            .bind(timed_with_usage.request_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("timed_out".into(), Some(1), Some(1)));
    let mut cancelled_with_usage = gateway
        .execute(
            principal(),
            request("usage-then-wait"),
            std::time::Instant::now(),
        )
        .await
        .unwrap();
    let cancelled_with_usage_id = cancelled_with_usage.request_id;
    assert!(matches!(
        cancelled_with_usage.next().await.unwrap().unwrap(),
        ProviderEvent::Usage(_)
    ));
    drop(cancelled_with_usage);
    gateway.shutdown().await;
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM requests WHERE id=$1")
        .bind(cancelled_with_usage_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(outcome, "cancelled");
    let usage = store.account_usage(account_id).await.unwrap();
    assert_eq!(usage.input_tokens + usage.output_tokens, 51);
    let totals_match: bool = sqlx::query_scalar("SELECT p.input_tokens=COALESCE(sum(r.input_tokens),0) AND p.output_tokens=COALESCE(sum(r.output_tokens),0) FROM provider_instances p LEFT JOIN requests r ON r.provider_id=p.id WHERE p.id=$1 GROUP BY p.id")
        .bind(&b).fetch_one(&pool).await.unwrap();
    assert!(totals_match);
    let expired_id = Uuid::new_v4();
    store
        .begin_request(llmproxy::store::NewRequest {
            id: expired_id,
            account_id,
            token_id: Some(token_id),
            ip_range_id: None,
            client_ip: None,
            requested_model: "test",
            provider_id: &b,
            upstream_model: "test",
            started_at: Utc::now() - chrono::Duration::minutes(2),
            deadline_at: Utc::now() - chrono::Duration::minutes(1),
        })
        .await
        .unwrap();
    store.reconcile_expired_requests().await.unwrap();
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM requests WHERE id=$1")
        .bind(expired_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(outcome, "interrupted");
    let output = metrics.render().unwrap();
    assert!(output.contains("llmproxy_requests_total"));
    assert!(
        output
            .lines()
            .filter(|line| line.starts_with("llmproxy_active_requests{"))
            .all(|line| line.ends_with(" 0"))
    );
    sqlx::query("UPDATE api_tokens SET revoked_at=now() WHERE id=$1")
        .bind(token_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.authenticate_token(&digest).await.unwrap().is_none());
    sqlx::query("UPDATE accounts SET enabled=false WHERE id=$1")
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .authenticate_token(&second_digest)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.authenticate_ip(client_ip).await.unwrap().is_none());
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    assert!(matches!(
        store.authenticate_token(&digest).await.err().unwrap().kind,
        llmproxy::core::ErrorKind::Persistence
    ));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn usage_migration_and_mutations_preserve_atomic_totals() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("usage_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = format!("SET search_path TO {schema}");
    let pool = PgPoolOptions::new()
        .max_connections(4)
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
    let initial = tempfile::tempdir().unwrap();
    std::fs::write(
        initial.path().join("0001_initial.sql"),
        include_str!("../migrations/0001_initial.sql"),
    )
    .unwrap();
    sqlx::migrate::Migrator::new(initial.path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let account = Uuid::new_v4();
    let token = Uuid::new_v4();
    let historical = Uuid::new_v4();
    sqlx::query("INSERT INTO accounts(id,label) VALUES ($1,'usage')")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO provider_instances(id,account_id) VALUES ('provider',$1)")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO api_tokens(id,account_id,label,token_hash) VALUES ($1,$2,'usage',$3)")
        .bind(token)
        .bind(account)
        .bind(vec![0u8; 32])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO requests(id,account_id,api_token_id,requested_model,provider_id,upstream_model,started_at,deadline_at,outcome,input_tokens) VALUES ($1,$2,$3,'test','provider','test',now(),now(),'failed',11)")
        .bind(historical).bind(account).bind(token).execute(&pool).await.unwrap();
    let store = PgStore::new(pool.clone());
    store.migrate().await.unwrap();
    let totals: (i64, i64) =
        sqlx::query_as("SELECT input_tokens,output_tokens FROM accounts WHERE id=$1")
            .bind(account)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(totals, (11, 0));
    let usage = store.account_usage(account).await.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 0));
    assert_eq!(
        store
            .provider_states(account, &["provider".into()])
            .await
            .unwrap()[0]
            .accumulated_tokens,
        11
    );

    let inserted = Uuid::new_v4();
    sqlx::query("INSERT INTO requests(id,account_id,api_token_id,requested_model,provider_id,upstream_model,started_at,deadline_at,outcome,input_tokens,output_tokens) VALUES ($1,$2,$3,'test','provider','test',now(),now(),'succeeded',2,4)")
        .bind(inserted).bind(account).bind(token).execute(&pool).await.unwrap();
    let usage = store.account_usage(account).await.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (13, 4));
    sqlx::query("DELETE FROM requests WHERE id=$1")
        .bind(inserted)
        .execute(&pool)
        .await
        .unwrap();
    let running = Uuid::new_v4();
    store
        .begin_request(llmproxy::store::NewRequest {
            id: running,
            account_id: account,
            token_id: Some(token),
            ip_range_id: None,
            client_ip: None,
            requested_model: "test",
            provider_id: "provider",
            upstream_model: "test",
            started_at: Utc::now(),
            deadline_at: Utc::now() + chrono::Duration::seconds(30),
        })
        .await
        .unwrap();
    let usage = Usage {
        input_tokens: 3,
        output_tokens: 5,
    };
    let (first, second) = tokio::join!(
        store.finish_request(running, "cancelled", None, Some(&usage)),
        store.finish_request(running, "cancelled", None, Some(&usage))
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let usage = store.account_usage(account).await.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (14, 5));
    sqlx::query("UPDATE requests SET input_tokens=7,output_tokens=2 WHERE id=$1")
        .bind(historical)
        .execute(&pool)
        .await
        .unwrap();
    let usage = store.account_usage(account).await.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 7));
    sqlx::query("DELETE FROM requests WHERE id=$1")
        .bind(historical)
        .execute(&pool)
        .await
        .unwrap();
    let usage = store.account_usage(account).await.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (3, 5));

    let unsaved = Uuid::new_v4();
    store
        .begin_request(llmproxy::store::NewRequest {
            id: unsaved,
            account_id: account,
            token_id: Some(token),
            ip_range_id: None,
            client_ip: None,
            requested_model: "test",
            provider_id: "provider",
            upstream_model: "test",
            started_at: Utc::now(),
            deadline_at: Utc::now() + chrono::Duration::seconds(30),
        })
        .await
        .unwrap();
    sqlx::query("ALTER TABLE provider_instances ADD CONSTRAINT test_counter_failure CHECK(input_tokens <= 3)").execute(&pool).await.unwrap();
    assert!(
        store
            .finish_request(
                unsaved,
                "succeeded",
                None,
                Some(&Usage {
                    input_tokens: 1,
                    output_tokens: 1
                })
            )
            .await
            .is_err()
    );
    let row: (String, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT outcome,input_tokens,output_tokens FROM requests WHERE id=$1")
            .bind(unsaved)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("running".into(), None, None));
    let usage = store.account_usage(account).await.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (3, 5));
    assert_eq!(
        store
            .provider_states(account, &["provider".into()])
            .await
            .unwrap()[0]
            .accumulated_tokens,
        8
    );
    sqlx::query("ALTER TABLE provider_instances DROP CONSTRAINT test_counter_failure")
        .execute(&pool)
        .await
        .unwrap();

    let now = Utc::now();
    store
        .save_quota(
            "provider",
            &[
                QuotaWindow {
                    key: "short".into(),
                    observed_at: now,
                    remaining_fraction: 0.25,
                    resets_at: None,
                },
                QuotaWindow {
                    key: "long".into(),
                    observed_at: now,
                    remaining_fraction: 0.75,
                    resets_at: None,
                },
            ],
        )
        .await
        .unwrap();
    assert!(
        store
            .save_quota(
                "provider",
                &[
                    QuotaWindow {
                        key: "short".into(),
                        observed_at: now + chrono::Duration::seconds(1),
                        remaining_fraction: 0.5,
                        resets_at: None
                    },
                    QuotaWindow {
                        key: "long".into(),
                        observed_at: now + chrono::Duration::seconds(1),
                        remaining_fraction: 2.0,
                        resets_at: None
                    },
                ]
            )
            .await
            .is_err()
    );
    store
        .save_quota(
            "provider",
            &[
                QuotaWindow {
                    key: "short".into(),
                    observed_at: now - chrono::Duration::seconds(1),
                    remaining_fraction: 1.0,
                    resets_at: None,
                },
                QuotaWindow {
                    key: "long".into(),
                    observed_at: now - chrono::Duration::seconds(1),
                    remaining_fraction: 1.0,
                    resets_at: None,
                },
            ],
        )
        .await
        .unwrap();
    let windows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT window_key,remaining_fraction FROM provider_quota ORDER BY window_key",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(windows, vec![("long".into(), 0.75), ("short".into(), 0.25)]);
    let writes = (0..16).map(|index| {
        let store = &store;
        async move {
            let mut windows = [
                QuotaWindow {
                    key: "short".into(),
                    observed_at: now + chrono::Duration::seconds(index + 2),
                    remaining_fraction: 0.5,
                    resets_at: None,
                },
                QuotaWindow {
                    key: "long".into(),
                    observed_at: now + chrono::Duration::seconds(index + 2),
                    remaining_fraction: 0.5,
                    resets_at: None,
                },
            ];
            if index % 2 == 0 {
                windows.reverse();
            }
            store.save_quota("provider", &windows).await.unwrap();
        }
    });
    futures_util::future::join_all(writes).await;
    let windows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT window_key,remaining_fraction FROM provider_quota ORDER BY window_key",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(windows, vec![("long".into(), 0.5), ("short".into(), 0.5)]);
    store.save_quota("provider", &[]).await.unwrap();
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
