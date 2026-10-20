use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use llmproxy::{
    core::{ErrorKind, EventStream, InferenceRequest, Provider, ProxyError},
    providers::maintain_credentials,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

struct MaintenanceProvider {
    calls: AtomicUsize,
    permits: Option<Arc<Semaphore>>,
    fails: bool,
}

#[async_trait]
impl Provider for MaintenanceProvider {
    async fn infer(
        &self,
        _: InferenceRequest,
        _: CancellationToken,
    ) -> Result<EventStream, ProxyError> {
        panic!("Maintenance must not invoke inference")
    }

    async fn refresh_credentials(&self) -> Result<(), ProxyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(permits) = &self.permits {
            permits.acquire().await.unwrap().forget();
        }
        if self.fails {
            Err(ProxyError::new(
                ErrorKind::UpstreamAuth,
                "fixture rejection",
            ))
        } else {
            Ok(())
        }
    }
}

#[tokio::test(start_paused = true)]
async fn maintenance_runs_without_requests_isolates_failures_and_drains_on_shutdown() {
    let permits = Arc::new(Semaphore::new(1));
    let slow = Arc::new(MaintenanceProvider {
        calls: AtomicUsize::new(0),
        permits: Some(permits.clone()),
        fails: false,
    });
    let failing = Arc::new(MaintenanceProvider {
        calls: AtomicUsize::new(0),
        permits: None,
        fails: true,
    });
    let healthy = Arc::new(MaintenanceProvider {
        calls: AtomicUsize::new(0),
        permits: None,
        fails: false,
    });
    let providers = HashMap::from([
        ("slow".into(), slow.clone() as Arc<dyn Provider>),
        ("failing".into(), failing.clone() as Arc<dyn Provider>),
        ("healthy".into(), healthy.clone() as Arc<dyn Provider>),
    ]);
    let stop = CancellationToken::new();
    let worker = tokio::spawn(maintain_credentials(providers, stop.clone()));
    tokio::task::yield_now().await;
    for provider in [&slow, &failing, &healthy] {
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert_eq!(healthy.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    for provider in [&slow, &failing, &healthy] {
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
    tokio::time::advance(Duration::from_secs(180)).await;
    tokio::task::yield_now().await;
    assert_eq!(healthy.calls.load(Ordering::SeqCst), 2);
    stop.cancel();
    tokio::task::yield_now().await;
    assert!(!worker.is_finished());
    permits.add_permits(1);
    worker.await.unwrap();
    assert_eq!(slow.calls.load(Ordering::SeqCst), 2);
}
