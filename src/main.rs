use std::{collections::HashMap, net::SocketAddr, process::ExitCode, sync::Arc, time::Duration};

use llmproxy::{
    api::{ApiState, router},
    config::{Config, ProviderConfig},
    core::Provider,
    gateway::{Gateway, LeastUsedPolicy},
    metrics::Metrics,
    providers::{
        claude::{ClaudeAuth, ClaudeProvider},
        codex::{CodexAuth, CodexProvider},
        maintain_credentials,
    },
    store::PgStore,
};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let (writer, logs) = llmproxy::logging::writer(std::io::stdout());
    tracing_subscriber::fmt()
        .json()
        .with_writer(writer)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("llmproxy=info")),
        )
        .init();
    let exit = if let Err(error) = run().await {
        tracing::error!(error = %error, "LLM proxy stopped");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    if let Err(error) = logs.finish() {
        eprintln!("Cannot flush LLM proxy logs: {error}");
        return ExitCode::FAILURE;
    }
    exit
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var_os("LLMPROXY_CONFIG").unwrap_or_else(|| "config.toml".into());
    let config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
    config.validate()?;
    let database_url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required")?;
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&database_url)
        .await
        .map_err(|_| "Cannot connect to PostgreSQL")?;
    let store = PgStore::new(pool);
    store.migrate().await?;
    store.reconcile_expired_requests().await?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(config.request_timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("llmproxy/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let metrics = Arc::new(Metrics::default());
    let mut providers = HashMap::<String, Arc<dyn Provider>>::new();
    for provider in config.providers {
        match provider {
            ProviderConfig::Codex { id } => {
                providers.insert(
                    id.clone(),
                    Arc::new(CodexProvider::new(
                        id,
                        store.clone(),
                        client.clone(),
                        CodexAuth::new(client.clone(), "https://auth.openai.com".into()),
                        "https://chatgpt.com/backend-api/codex/responses".into(),
                        metrics.clone(),
                    )),
                );
            }
            ProviderConfig::Claude { id, executable } => {
                providers.insert(
                    id.clone(),
                    Arc::new(ClaudeProvider::new(
                        id,
                        executable,
                        store.clone(),
                        ClaudeAuth::new(
                            client.clone(),
                            "https://platform.claude.com/v1/oauth/token".into(),
                        ),
                        metrics.clone(),
                    )),
                );
            }
        }
    }
    let maintenance_providers = providers.clone();
    let gateway = Arc::new(Gateway::new(
        store.clone(),
        providers,
        Arc::new(LeastUsedPolicy),
        metrics.clone(),
        config.max_concurrent_requests,
        Duration::from_secs(config.request_timeout_seconds),
    ));
    let app = router(ApiState {
        gateway: gateway.clone(),
        store: store.clone(),
        metrics,
    });
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let maintenance_stop = CancellationToken::new();
    let maintenance = tokio::spawn(maintain_credentials(
        maintenance_providers,
        maintenance_stop.clone(),
    ));
    let reconciliation = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Err(error) = store.reconcile_expired_requests().await {
                tracing::error!(
                    error_kind = error.kind.as_str(),
                    "Request reconciliation failed"
                );
            }
        }
    });
    let shutdown_maintenance = maintenance_stop.clone();
    tracing::info!(listen=%config.listen, "LLM proxy started");
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        #[cfg(unix)]
        {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        shutdown_maintenance.cancel();
        tracing::info!("LLM proxy shutting down");
    })
    .await;
    maintenance_stop.cancel();
    let maintenance_result = maintenance.await;
    gateway.shutdown().await;
    reconciliation.abort();
    let _ = reconciliation.await;
    maintenance_result?;
    result?;
    Ok(())
}
