mod auth;
pub mod oauth;

use std::{collections::HashSet, sync::Arc};

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    middleware,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    core::{ErrorKind, ProviderKind, ProxyError},
    store::{
        PgStore,
        management::{
            ManagedAccount, ManagedApiToken, ManagedProvider, ManagementSession,
            RoutingConfiguration, TokenMetrics,
        },
    },
};

#[derive(Clone)]
pub struct ManagementState {
    pub(crate) store: PgStore,
    pub(crate) password_jobs: Arc<Semaphore>,
    pub(crate) cookie_secure: bool,
    pub(crate) oauth: Arc<oauth::OAuthManager>,
}

impl ManagementState {
    pub fn new(store: PgStore, oauth: Arc<oauth::OAuthManager>, cookie_secure: bool) -> Self {
        Self {
            store,
            password_jobs: Arc::new(Semaphore::new(4)),
            cookie_secure,
            oauth,
        }
    }
}

pub fn router(state: ManagementState) -> Router {
    let authenticated = Router::new()
        .route("/session", delete(auth::logout))
        .route("/account", get(account).patch(update_account))
        .route("/api-tokens", get(api_tokens).post(create_api_token))
        .route("/api-tokens/{id}", delete(revoke_api_token))
        .route("/api-tokens/{id}/metrics", get(token_metrics))
        .route("/providers", get(providers).post(register_provider))
        .route("/providers/{id}", get(provider).patch(update_provider))
        .route("/routing", get(routing).put(replace_routing))
        .merge(oauth::router())
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::session));
    Router::new()
        .nest(
            "/management",
            Router::new()
                .route("/accounts", post(auth::signup))
                .route("/sessions", post(auth::login))
                .merge(authenticated)
                .layer(middleware::from_fn(auth::management_headers))
                .layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .with_state(state)
}

pub(super) fn read_json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ProxyError> {
    body.map(|Json(body)| body)
        .map_err(|_| ProxyError::new(ErrorKind::InvalidRequest, "Invalid JSON request"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelRequest {
    label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRequest {
    kind: ProviderKind,
    label: String,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPatch {
    label: Option<String>,
    enabled: Option<bool>,
}

#[derive(Serialize)]
struct IssuedApiToken {
    #[serde(flatten)]
    metadata: ManagedApiToken,
    token: String,
}

fn label(value: &str) -> Result<&str, ProxyError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 {
        return Err(ProxyError::new(
            ErrorKind::InvalidRequest,
            "Label must contain 1 to 256 bytes",
        ));
    }
    Ok(value)
}

async fn account(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
) -> Result<Json<ManagedAccount>, ProxyError> {
    Ok(Json(state.store.managed_account(session.account_id).await?))
}

async fn update_account(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    body: Result<Json<LabelRequest>, JsonRejection>,
) -> Result<Json<ManagedAccount>, ProxyError> {
    let body = read_json(body)?;
    Ok(Json(
        state
            .store
            .update_account_label(session.account_id, label(&body.label)?)
            .await?,
    ))
}

async fn api_tokens(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
) -> Result<Json<Vec<ManagedApiToken>>, ProxyError> {
    Ok(Json(state.store.api_tokens(session.account_id).await?))
}

async fn create_api_token(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    body: Result<Json<LabelRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<IssuedApiToken>), ProxyError> {
    let body = read_json(body)?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let metadata = state
        .store
        .create_api_token(
            session.account_id,
            label(&body.label)?,
            &Sha256::digest(token.as_bytes()),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedApiToken { metadata, token }),
    ))
}

async fn revoke_api_token(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ProxyError> {
    state.store.revoke_api_token(session.account_id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn token_metrics(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    Path(id): Path<Uuid>,
) -> Result<Json<TokenMetrics>, ProxyError> {
    Ok(Json(
        state.store.token_metrics(session.account_id, id).await?,
    ))
}

async fn providers(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
) -> Result<Json<Vec<ManagedProvider>>, ProxyError> {
    Ok(Json(
        state.store.managed_providers(session.account_id).await?,
    ))
}

async fn provider(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    Path(id): Path<String>,
) -> Result<Json<ManagedProvider>, ProxyError> {
    Ok(Json(
        state.store.owned_provider(session.account_id, &id).await?,
    ))
}

async fn register_provider(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    body: Result<Json<ProviderRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ManagedProvider>), ProxyError> {
    let body = read_json(body)?;
    Ok((
        StatusCode::CREATED,
        Json(
            state
                .store
                .register_provider(
                    session.account_id,
                    body.kind,
                    label(&body.label)?,
                    body.enabled.unwrap_or(true),
                )
                .await?,
        ),
    ))
}

async fn update_provider(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    Path(id): Path<String>,
    body: Result<Json<ProviderPatch>, JsonRejection>,
) -> Result<Json<ManagedProvider>, ProxyError> {
    let body = read_json(body)?;
    if body.label.is_none() && body.enabled.is_none() {
        return Err(ProxyError::new(
            ErrorKind::InvalidRequest,
            "Provider patch is empty",
        ));
    }
    let label = body.label.as_deref().map(label).transpose()?;
    Ok(Json(
        state
            .store
            .update_provider(session.account_id, &id, label, body.enabled)
            .await?,
    ))
}

async fn routing(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
) -> Result<Json<RoutingConfiguration>, ProxyError> {
    Ok(Json(
        state
            .store
            .routing_configuration(session.account_id)
            .await?,
    ))
}

async fn replace_routing(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    body: Result<Json<RoutingConfiguration>, JsonRejection>,
) -> Result<Json<RoutingConfiguration>, ProxyError> {
    let body = read_json(body)?;
    if body.routes.len() > 1000 {
        return Err(ProxyError::new(
            ErrorKind::InvalidRequest,
            "At most 1000 model routes are supported",
        ));
    }
    let mut mappings = HashSet::new();
    for route in &body.routes {
        if [&route.model, &route.provider_id, &route.upstream_model]
            .iter()
            .any(|value| value.is_empty() || value.len() > 256 || value.trim() != value.as_str())
        {
            return Err(ProxyError::new(
                ErrorKind::InvalidRequest,
                "Route names must contain 1 to 256 bytes without surrounding whitespace",
            ));
        }
        if !mappings.insert((&route.model, &route.provider_id)) {
            return Err(ProxyError::new(
                ErrorKind::InvalidRequest,
                "Duplicate model/provider mapping",
            ));
        }
    }
    state
        .store
        .replace_routing(session.account_id, &body)
        .await?;
    Ok(Json(body))
}
