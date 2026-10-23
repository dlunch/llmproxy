use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use llmproxy::{
    core::{ErrorKind, EventStream, InferenceRequest, Provider, ProviderKind, ProxyError},
    providers::maintain_credentials,
    store::PgStore,
};
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct MaintenanceProvider {
    calls: Mutex<HashMap<String, usize>>,
    permits: Semaphore,
}

#[async_trait]
impl Provider for MaintenanceProvider {
    async fn infer(
        &self,
        _: &str,
        _: InferenceRequest,
        _: CancellationToken,
    ) -> Result<EventStream, ProxyError> {
        panic!("Maintenance must not invoke inference")
    }

    async fn refresh_credentials(&self, id: &str) -> Result<(), ProxyError> {
        *self.calls.lock().unwrap().entry(id.to_owned()).or_default() += 1;
        if id == "slow" || id.starts_with("blocked") {
            self.permits.acquire().await.unwrap().forget();
        }
        if id == "failing" {
            Err(ProxyError::new(
                ErrorKind::UpstreamAuth,
                "fixture rejection",
            ))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn maintenance_observes_registration_isolates_failures_and_drains_on_shutdown() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("maintenance_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = format!("SET search_path TO {schema}");
    let pool = PgPoolOptions::new()
        .max_connections(2)
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
    sqlx::query("INSERT INTO accounts (id, label) VALUES ($1, 'maintenance')")
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    for id in ["slow", "failing", "healthy", "disabled"] {
        sqlx::query("INSERT INTO provider_instances (id, account_id, kind, label, enabled) VALUES ($1, $2, 'codex', $1, $3)")
            .bind(id).bind(account_id).bind(id != "disabled").execute(&pool).await.unwrap();
    }
    let provider = Arc::new(MaintenanceProvider {
        calls: Mutex::new(HashMap::new()),
        permits: Semaphore::new(1),
    });
    let providers = HashMap::from([(ProviderKind::Codex, provider.clone() as Arc<dyn Provider>)]);
    let stop = CancellationToken::new();
    let worker = tokio::spawn(maintain_credentials(store, providers, stop.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while provider.calls.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(!provider.calls.lock().unwrap().contains_key("disabled"));
    sqlx::query("INSERT INTO provider_instances (id, account_id, kind, label) VALUES ('new', $1, 'codex', 'new')")
        .bind(account_id).execute(&pool).await.unwrap();
    // Advance only the scheduler; PostgreSQL I/O keeps a running clock.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::time::resume();
    assert_eq!(provider.calls.lock().unwrap()["healthy"], 1);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ready = {
                let calls = provider.calls.lock().unwrap();
                calls.get("new") == Some(&1)
                    && calls["healthy"] == 2
                    && calls["failing"] == 2
                    && calls["slow"] == 2
            };
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(180)).await;
    tokio::time::resume();
    assert_eq!(provider.calls.lock().unwrap()["healthy"], 2);
    stop.cancel();
    tokio::task::yield_now().await;
    assert!(!worker.is_finished());
    provider.permits.add_permits(1);
    worker.await.unwrap();
    assert_eq!(provider.calls.lock().unwrap()["slow"], 2);
    sqlx::query("UPDATE provider_instances SET enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO provider_instances (id, account_id, kind, label) SELECT 'blocked-'||n, $1, 'codex', 'blocked' FROM generate_series(1,8) n")
        .bind(account_id).execute(&pool).await.unwrap();
    provider.calls.lock().unwrap().clear();
    let stop = CancellationToken::new();
    let worker = tokio::spawn(maintain_credentials(
        PgStore::new(pool.clone()),
        HashMap::from([(ProviderKind::Codex, provider.clone() as Arc<dyn Provider>)]),
        stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while provider.calls.lock().unwrap().len() < 4 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(provider.calls.lock().unwrap().len(), 4);
    stop.cancel();
    provider.permits.add_permits(4);
    worker.await.unwrap();
    assert_eq!(
        provider.calls.lock().unwrap().len(),
        4,
        "shutdown must not start queued refreshes"
    );
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
