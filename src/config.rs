use std::{net::SocketAddr, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub max_concurrent_requests: usize,
    pub request_timeout_seconds: u64,
    pub claude_executable: PathBuf,
    pub cookie_secure: bool,
    pub dashboard_origin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".parse().expect("static address"),
            max_concurrent_requests: 16,
            request_timeout_seconds: 120,
            claude_executable: "/usr/local/bin/claude".into(),
            cookie_secure: true,
            dashboard_origin: None,
        }
    }
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
        if !self.claude_executable.is_absolute() {
            return Err("Claude executable must be an absolute path".into());
        }
        if let Some(origin) = &self.dashboard_origin {
            let url = reqwest::Url::parse(origin).map_err(|_| "Invalid dashboard origin")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.origin().ascii_serialization() != *origin
            {
                return Err(
                    "dashboard_origin must be an HTTP(S) origin without a path or trailing slash"
                        .into(),
                );
            }
        }
        Ok(())
    }
}
