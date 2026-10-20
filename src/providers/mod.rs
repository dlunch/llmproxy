pub mod claude;
pub mod codex;

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use futures_util::future::join_all;
use reqwest::Response;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::core::{ErrorKind, Provider, ProxyError};

pub async fn maintain_credentials(
    providers: HashMap<String, Arc<dyn Provider>>,
    stop: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            _ = interval.tick() => {},
        }
        // Once refresh starts, drain it through persistence even during shutdown.
        join_all(providers.iter().map(|(provider_id, provider)| async move {
            if let Err(error) = provider.refresh_credentials().await {
                tracing::warn!(
                    provider_id,
                    error_kind = error.kind.as_str(),
                    "OAuth credential refresh failed"
                );
            }
        }))
        .await;
    }
}

pub(crate) fn credentials_need_refresh(
    expires_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
) -> bool {
    let now = Utc::now();
    match expires_at {
        Some(expiry) => {
            expiry <= now + chrono::Duration::minutes(5)
                || updated_at <= now - chrono::Duration::hours(24)
        }
        None => updated_at <= now - chrono::Duration::hours(1),
    }
}

pub(crate) async fn auth_json(mut response: Response) -> Result<Value, ProxyError> {
    if !response.status().is_success() {
        return Err(ProxyError::new(
            ErrorKind::UpstreamAuth,
            "OAuth request rejected",
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ProxyError::new(ErrorKind::UpstreamAuth, "OAuth response failed"))?
    {
        if body.len() + chunk.len() > 1024 * 1024 {
            return Err(ProxyError::new(
                ErrorKind::UpstreamAuth,
                "OAuth response too large",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| ProxyError::new(ErrorKind::UpstreamAuth, "Invalid OAuth response"))
}
