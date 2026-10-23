use std::time::{Duration, Instant};

use prometheus_client::{
    encoding::text::encode,
    metrics::{counter::Counter, family::Family, gauge::Gauge, histogram::Histogram},
    registry::Registry,
};

use crate::core::{ErrorKind, Usage};

type Labels = Vec<(&'static str, String)>;

const LATENCY_BUCKETS: [f64; 16] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0, 15.0, 30.0, 60.0, 180.0,
    600.0,
];

#[derive(Clone, Copy)]
pub enum Stage {
    Authentication,
    Routing,
    CredentialRead,
    CredentialLock,
    CredentialRefresh,
    MaintenanceCredentialLock,
    MaintenanceCredentialRefresh,
    ProviderSetup,
    FirstText,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet)]
struct StageLabel {
    stage: &'static str,
}

pub struct StageTimer<'a> {
    metrics: &'a Metrics,
    stage: Stage,
    started: Instant,
}

impl Drop for StageTimer<'_> {
    fn drop(&mut self) {
        self.metrics
            .observe_stage(self.stage, self.started.elapsed());
    }
}

pub struct Metrics {
    registry: Registry,
    requests: Family<Labels, Counter>,
    failures: Family<Labels, Counter>,
    rejected: Family<Labels, Counter>,
    active: Family<Labels, Gauge>,
    tokens: Family<Labels, Counter>,
    latency: Family<Labels, Histogram>,
    stages: Family<StageLabel, Histogram>,
}

impl Default for Metrics {
    fn default() -> Self {
        let mut registry = Registry::default();
        let requests = Family::default();
        let failures = Family::default();
        let rejected = Family::default();
        let active = Family::default();
        let tokens = Family::default();
        let latency =
            Family::<Labels, Histogram>::new_with_constructor(|| Histogram::new(LATENCY_BUCKETS));
        let stages = Family::<StageLabel, Histogram>::new_with_constructor(|| {
            Histogram::new(LATENCY_BUCKETS)
        });
        registry.register(
            "llmproxy_rejected_requests",
            "Authenticated inference requests rejected before durable admission",
            rejected.clone(),
        );
        registry.register(
            "llmproxy_requests",
            "Finalized inference requests",
            requests.clone(),
        );
        registry.register(
            "llmproxy_provider_errors",
            "Failed inference requests",
            failures.clone(),
        );
        registry.register(
            "llmproxy_active_requests",
            "Active inference requests",
            active.clone(),
        );
        registry.register(
            "llmproxy_tokens",
            "Provider-reported tokens",
            tokens.clone(),
        );
        registry.register(
            "llmproxy_request_duration_seconds",
            "Admitted inference lifecycle from API handler entry through durable finalization",
            latency.clone(),
        );
        registry.register(
            "llmproxy_stage_duration_seconds",
            "Request stage duration; FirstText starts at API handler entry and other stages may overlap",
            stages.clone(),
        );
        Self {
            registry,
            requests,
            failures,
            rejected,
            active,
            tokens,
            latency,
            stages,
        }
    }
}

impl Metrics {
    pub fn time_stage(&self, stage: Stage) -> StageTimer<'_> {
        StageTimer {
            metrics: self,
            stage,
            started: Instant::now(),
        }
    }

    pub fn observe_stage(&self, stage: Stage, elapsed: Duration) {
        self.stages
            .get_or_create(&StageLabel {
                stage: match stage {
                    Stage::Authentication => "authentication",
                    Stage::Routing => "routing",
                    Stage::CredentialRead => "credential_read",
                    Stage::CredentialLock => "credential_lock",
                    Stage::CredentialRefresh => "credential_refresh",
                    Stage::MaintenanceCredentialLock => "maintenance_credential_lock",
                    Stage::MaintenanceCredentialRefresh => "maintenance_credential_refresh",
                    Stage::ProviderSetup => "provider_setup",
                    Stage::FirstText => "first_text",
                },
            })
            .observe(elapsed.as_secs_f64());
    }

    pub fn started(&self, provider: &str, api_token_id: Option<uuid::Uuid>) {
        self.active
            .get_or_create(&vec![
                ("provider", provider.into()),
                (
                    "api_token_id",
                    api_token_id.map(|id| id.to_string()).unwrap_or_default(),
                ),
                (
                    "auth_source",
                    if api_token_id.is_some() {
                        "api_token"
                    } else {
                        "ip"
                    }
                    .into(),
                ),
            ])
            .inc();
    }

    pub fn rejected(&self, api_token_id: Option<uuid::Uuid>, error: ErrorKind) {
        self.rejected
            .get_or_create(&vec![
                (
                    "api_token_id",
                    api_token_id.map(|id| id.to_string()).unwrap_or_default(),
                ),
                (
                    "auth_source",
                    if api_token_id.is_some() {
                        "api_token"
                    } else {
                        "ip"
                    }
                    .into(),
                ),
                ("error_kind", error.as_str().into()),
            ])
            .inc();
    }

    pub fn finished(
        &self,
        provider: &str,
        api_token_id: Option<uuid::Uuid>,
        outcome: &str,
        elapsed: Duration,
        usage: Option<&Usage>,
    ) {
        let mut labels = vec![
            ("provider", provider.into()),
            (
                "api_token_id",
                api_token_id.map(|id| id.to_string()).unwrap_or_default(),
            ),
            (
                "auth_source",
                if api_token_id.is_some() {
                    "api_token"
                } else {
                    "ip"
                }
                .into(),
            ),
        ];
        self.active.get_or_create(&labels).dec();
        self.latency
            .get_or_create(&labels)
            .observe(elapsed.as_secs_f64());
        labels.push(("outcome", outcome.into()));
        self.requests.get_or_create(&labels).inc();
        if outcome == "failed" || outcome == "timed_out" {
            self.failures.get_or_create(&labels).inc();
        }
        labels.pop();
        if let Some(usage) = usage {
            for (direction, count) in [
                ("input", usage.input_tokens),
                ("output", usage.output_tokens),
            ] {
                labels.push(("direction", direction.into()));
                self.tokens.get_or_create(&labels).inc_by(count);
                labels.pop();
            }
        }
    }

    pub fn render(&self) -> Result<String, std::fmt::Error> {
        let mut output = String::new();
        encode(&mut output, &self.registry)?;
        Ok(output)
    }
}
