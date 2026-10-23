use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use futures_util::{Stream, StreamExt};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

pub use crate::store::{ProviderState, QuotaWindow};
use crate::{
    core::{
        Credential, ErrorKind, InferenceRequest, Principal, Provider, ProviderEvent, ProviderKind,
        ProxyError, RoutingContext, RoutingStrategy, Usage,
    },
    metrics::{Metrics, Stage},
    store::{NewRequest, PgStore},
};

pub use crate::core::Route;

pub trait RoutingPolicy: Send + Sync {
    fn select(
        &self,
        request: &InferenceRequest,
        context: &RoutingContext,
        routes: &[Route],
        states: &[ProviderState],
    ) -> Result<usize, ProxyError>;
}

pub struct ConfiguredPolicy;

impl RoutingPolicy for ConfiguredPolicy {
    fn select(
        &self,
        request: &InferenceRequest,
        context: &RoutingContext,
        routes: &[Route],
        states: &[ProviderState],
    ) -> Result<usize, ProxyError> {
        let now = Utc::now();
        let mut model_exists = false;
        let mut selected: Option<(usize, &ProviderState)> = None;
        for (index, route) in routes
            .iter()
            .enumerate()
            .filter(|(_, route)| route.model == request.model)
        {
            model_exists = true;
            if let Some(state) = states
                .iter()
                .find(|state| state.provider_id == route.provider_id)
            {
                let exhausted = state.quota.iter().any(|window| {
                    window.remaining_fraction <= 0.0
                        && now.signed_duration_since(window.observed_at)
                            < chrono::Duration::minutes(5)
                        && window.resets_at.is_none_or(|reset| reset > now)
                });
                if !exhausted
                    && selected.is_none_or(|(previous_index, previous)| match context.strategy {
                        RoutingStrategy::LeastUsed => {
                            (state.accumulated_tokens, &state.provider_id, index)
                                < (
                                    previous.accumulated_tokens,
                                    &previous.provider_id,
                                    previous_index,
                                )
                        }
                        RoutingStrategy::Priority => {
                            (route.priority, &state.provider_id, index)
                                < (
                                    routes[previous_index].priority,
                                    &previous.provider_id,
                                    previous_index,
                                )
                        }
                    })
                {
                    selected = Some((index, state));
                }
            }
        }
        selected.map(|(index, _)| index).ok_or_else(|| {
            if model_exists {
                ProxyError::new(ErrorKind::QuotaExceeded, "No provider has available quota")
            } else {
                ProxyError::new(ErrorKind::ModelNotFound, "Unknown model")
            }
        })
    }
}

pub struct GatewayStream {
    pub request_id: Uuid,
    pub created_at: DateTime<Utc>,
    receiver: mpsc::Receiver<Result<ProviderEvent, ProxyError>>,
    cancellation: CancellationToken,
}

impl Stream for GatewayStream {
    type Item = Result<ProviderEvent, ProxyError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

impl Drop for GatewayStream {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub struct Gateway {
    store: PgStore,
    providers: HashMap<ProviderKind, Arc<dyn Provider>>,
    policy: Arc<dyn RoutingPolicy>,
    metrics: Arc<Metrics>,
    concurrency: Arc<Semaphore>,
    timeout: Duration,
    tasks: TaskTracker,
}

impl Gateway {
    pub fn new(
        store: PgStore,
        providers: HashMap<ProviderKind, Arc<dyn Provider>>,
        policy: Arc<dyn RoutingPolicy>,
        metrics: Arc<Metrics>,
        max_concurrent: usize,
        timeout: Duration,
    ) -> Self {
        Self {
            store,
            providers,
            policy,
            metrics,
            concurrency: Arc::new(Semaphore::new(max_concurrent)),
            timeout,
            tasks: TaskTracker::new(),
        }
    }

    pub async fn execute(
        &self,
        principal: Principal,
        mut request: InferenceRequest,
        started: Instant,
    ) -> Result<GatewayStream, ProxyError> {
        let permit = self.concurrency.clone().try_acquire_owned().map_err(|_| {
            ProxyError::new(ErrorKind::Unavailable, "Proxy concurrency limit reached")
        })?;
        let routing_timer = self.metrics.time_stage(Stage::Routing);
        let (states, (context, mut routes)) = tokio::try_join!(
            self.store.provider_states(principal.account_id),
            self.store
                .routing_snapshot(principal.account_id, &request.model),
        )?;
        let selected = self.policy.select(&request, &context, &routes, &states)?;
        let route = routes.swap_remove(selected);
        drop(routing_timer);
        let provider = self
            .providers
            .get(&route.provider_kind)
            .ok_or_else(|| ProxyError::new(ErrorKind::Unavailable, "Provider unavailable"))?
            .clone();
        let created_at = Utc::now();
        let request_id = Uuid::new_v4();
        let deadline = tokio::time::Instant::now() + self.timeout;
        let (token_id, ip_range_id, client_ip) = match principal.credential {
            Credential::ApiToken(id) => (Some(id), None, None),
            Credential::AllowedIp { range_id, address } => (None, Some(range_id), Some(address)),
        };
        self.store
            .begin_request(NewRequest {
                id: request_id,
                account_id: principal.account_id,
                token_id,
                ip_range_id,
                client_ip,
                requested_model: &request.model,
                provider_id: &route.provider_id,
                upstream_model: &route.upstream_model,
                started_at: created_at,
                deadline_at: created_at + self.timeout,
            })
            .await?;
        request.model = route.upstream_model;
        let provider_id = route.provider_id;
        let store = self.store.clone();
        let metrics = self.metrics.clone();
        let (sender, receiver) = mpsc::channel(16);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        metrics.started(&provider_id, token_id);
        self.tasks.spawn(async move {
            let _permit = permit;
            let mut usage = None;
            let mut output_bytes = 0usize;
            let mut first_text = false;
            let result = async {
                let cancelled = task_cancellation.cancelled();
                let timed_out = tokio::time::sleep_until(deadline);
                tokio::pin!(cancelled, timed_out);
                let preparation_timer = metrics.time_stage(Stage::ProviderSetup);
                let mut stream = tokio::select! {
                    biased;
                    _ = &mut cancelled => return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
                    _ = &mut timed_out => return Err(ProxyError::new(ErrorKind::Timeout, "Request timed out")),
                    result = provider.infer(&provider_id, request, task_cancellation.clone()) => result?,
                };
                drop(preparation_timer);
                loop {
                    let event = tokio::select! {
                        biased;
                        _ = &mut cancelled => return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
                        _ = &mut timed_out => return Err(ProxyError::new(ErrorKind::Timeout, "Request timed out")),
                        event = stream.next() => event.ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Upstream ended before completion"))??,
                    };
                    match &event {
                        ProviderEvent::Finished(_) => return Ok(event),
                        ProviderEvent::Usage(value) => {
                            if value.input_tokens > i64::MAX as u64 || value.output_tokens > i64::MAX as u64 {
                                return Err(ProxyError::new(ErrorKind::Upstream, "Invalid upstream token count"));
                            }
                            usage = Some(Usage { input_tokens: value.input_tokens, output_tokens: value.output_tokens });
                        }
                        ProviderEvent::ToolCallDelta { id, name, arguments, .. } => {
                            output_bytes = output_bytes.saturating_add(arguments.len())
                                .saturating_add(id.as_ref().map_or(0, String::len))
                                .saturating_add(name.as_ref().map_or(0, String::len));
                        }
                        ProviderEvent::WebSearch { id, query, .. } => {
                            output_bytes = output_bytes.saturating_add(id.len())
                                .saturating_add(query.as_ref().map_or(0, String::len));
                        }
                        ProviderEvent::Citation { url, title, .. } => {
                            output_bytes = output_bytes.saturating_add(url.len()).saturating_add(title.len());
                        }
                        ProviderEvent::TextDelta(text) => {
                            if !first_text && !text.is_empty() {
                                metrics.observe_stage(Stage::FirstText, started.elapsed());
                                first_text = true;
                            }
                            output_bytes = output_bytes.saturating_add(text.len());
                        }
                    }
                    if output_bytes > 8 * 1024 * 1024 {
                        return Err(ProxyError::new(ErrorKind::Upstream, "Upstream output exceeded limit"));
                    }
                    tokio::select! {
                        biased;
                        _ = &mut cancelled => return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
                        _ = &mut timed_out => return Err(ProxyError::new(ErrorKind::Timeout, "Request timed out")),
                        sent = sender.send(Ok(event)) => {
                            if sent.is_err() { return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")); }
                        }
                    }
                }
            }.await;
            task_cancellation.cancel();
            let outcome = match &result {
                Ok(_) => "succeeded",
                Err(error) => match error.kind {
                    ErrorKind::Cancelled => "cancelled",
                    ErrorKind::Timeout => "timed_out",
                    _ => "failed",
                },
            };
            let saved = tokio::time::timeout(
                Duration::from_secs(5),
                store.finish_request(
                    request_id,
                    outcome,
                    result.as_ref().err().map(|e| e.kind),
                    usage.as_ref(),
                ),
            )
            .await;
            let final_result = match saved {
                Ok(Ok(())) => result,
                _ => {
                    tracing::error!(%request_id, "Request finalization failed; reconciliation required");
                    Err(ProxyError::new(
                        ErrorKind::Persistence,
                        "Request accounting could not be saved",
                    ))
                }
            };
            let metric_outcome = if matches!(&final_result, Err(error) if matches!(error.kind, ErrorKind::Persistence))
            {
                "failed"
            } else {
                outcome
            };
            metrics.finished(
                &provider_id,
                token_id,
                metric_outcome,
                started.elapsed(),
                usage.as_ref(),
            );
            // A stalled reader cannot retain a completed lifecycle task indefinitely.
            let _ = tokio::time::timeout(Duration::from_secs(1), sender.send(final_result)).await;
        });
        Ok(GatewayStream {
            request_id,
            created_at,
            receiver,
            cancellation,
        })
    }

    pub async fn shutdown(&self) {
        self.tasks.close();
        self.tasks.wait().await;
        for provider in self.providers.values() {
            provider.shutdown().await;
        }
    }
}
