use std::time::Duration;

use prometheus_client::{
    encoding::text::encode,
    metrics::{counter::Counter, family::Family, gauge::Gauge, histogram::Histogram},
    registry::Registry,
};

use crate::core::Usage;

pub struct Metrics {
    registry: Registry,
    requests: Family<Vec<(String, String)>, Counter>,
    failures: Family<Vec<(String, String)>, Counter>,
    active: Family<Vec<(String, String)>, Gauge>,
    tokens: Family<Vec<(String, String)>, Counter>,
    latency: Histogram,
}

impl Default for Metrics {
    fn default() -> Self {
        let mut registry = Registry::default();
        let requests = Family::default();
        let failures = Family::default();
        let active = Family::default();
        let tokens = Family::default();
        let latency = Histogram::new([0.1, 0.5, 1.0, 5.0, 15.0, 30.0, 60.0, 180.0, 600.0]);
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
            "Inference request duration",
            latency.clone(),
        );
        Self {
            registry,
            requests,
            failures,
            active,
            tokens,
            latency,
        }
    }
}

impl Metrics {
    pub fn started(&self, provider: &str) {
        self.active
            .get_or_create(&vec![("provider".into(), provider.into())])
            .inc();
    }

    pub fn finished(
        &self,
        provider: &str,
        outcome: &str,
        elapsed: Duration,
        usage: Option<&Usage>,
    ) {
        let labels = vec![
            ("provider".into(), provider.into()),
            ("outcome".into(), outcome.into()),
        ];
        self.requests.get_or_create(&labels).inc();
        if outcome == "failed" || outcome == "timed_out" {
            self.failures.get_or_create(&labels).inc();
        }
        self.active
            .get_or_create(&vec![("provider".into(), provider.into())])
            .dec();
        self.latency.observe(elapsed.as_secs_f64());
        if let Some(usage) = usage {
            for (direction, count) in [
                ("input", usage.input_tokens),
                ("output", usage.output_tokens),
            ] {
                self.tokens
                    .get_or_create(&vec![
                        ("provider".into(), provider.into()),
                        ("direction".into(), direction.into()),
                    ])
                    .inc_by(count);
            }
        }
    }

    pub fn render(&self) -> Result<String, std::fmt::Error> {
        let mut output = String::new();
        encode(&mut output, &self.registry)?;
        Ok(output)
    }
}
