use std::{collections::HashSet, net::SocketAddr, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub max_concurrent_requests: usize,
    pub request_timeout_seconds: u64,
    pub providers: Vec<ProviderConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".parse().expect("static address"),
            max_concurrent_requests: 16,
            request_timeout_seconds: 120,
            providers: vec![],
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderConfig {
    Codex { id: String },
    Claude { id: String, executable: PathBuf },
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_concurrent_requests == 0
            || self.max_concurrent_requests > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err("max_concurrent_requests is outside the supported range".into());
        }
        if !(1..=86400).contains(&self.request_timeout_seconds) {
            return Err("request_timeout_seconds must be between 1 and 86400".into());
        }
        let mut ids = HashSet::new();
        for provider in &self.providers {
            let id = match provider {
                ProviderConfig::Codex { id } => id,
                ProviderConfig::Claude { id, executable } => {
                    if !executable.is_absolute() {
                        return Err("Claude executable must be an absolute path".into());
                    }
                    id
                }
            };
            if id.is_empty() || !ids.insert(id) {
                return Err("Provider IDs must be nonempty and unique".into());
            }
        }
        Ok(())
    }
}
