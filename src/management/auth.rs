use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use axum::{
    Extension, Json,
    extract::{Request, State, rejection::JsonRejection},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{ManagementState, read_json};
use crate::{
    core::{ErrorKind, ProxyError},
    store::management::{ManagedAccount, ManagementSession},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignupRequest {
    email: String,
    password: String,
    label: String,
    signup_code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    account: ManagedAccount,
    expires_at: chrono::DateTime<Utc>,
}

fn credentials(email: &str, password: &str) -> Result<String, ProxyError> {
    let email = email.trim().to_ascii_lowercase();
    let valid_address = email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && !domain.is_empty() && !domain.contains('@')
    });
    if email.len() > 254
        || !email.is_ascii()
        || email
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || !valid_address
    {
        return Err(ProxyError::new(
            ErrorKind::InvalidRequest,
            "Invalid email address",
        ));
    }
    if !(8..=1024).contains(&password.len()) {
        return Err(ProxyError::new(
            ErrorKind::InvalidRequest,
            "Password must contain 8 to 1024 bytes",
        ));
    }
    Ok(email)
}

pub(super) async fn management_headers(request: Request, next: Next) -> Response {
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) && request
        .headers()
        .get("X-LLMProxy-Management")
        .is_none_or(|value| value != "1")
    {
        return ProxyError::new(
            ErrorKind::InvalidRequest,
            "Management mutation header is required",
        )
        .into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) async fn session(
    State(state): State<ManagementState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ProxyError> {
    let mut token = None;
    for value in request.headers().get_all(header::COOKIE) {
        let value = value
            .to_str()
            .map_err(|_| ProxyError::new(ErrorKind::Unauthorized, "Invalid management session"))?;
        for item in value.split(';') {
            if let Some(("llmproxy_session", value)) = item.trim().split_once('=') {
                if token.is_some()
                    || value.len() != 64
                    || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(ProxyError::new(
                        ErrorKind::Unauthorized,
                        "Invalid management session",
                    ));
                }
                token = Some(value);
            }
        }
    }
    let token = token
        .ok_or_else(|| ProxyError::new(ErrorKind::Unauthorized, "Management login required"))?;
    let session = state
        .store
        .authenticate_management_session(&Sha256::digest(token.as_bytes()))
        .await?
        .ok_or_else(|| ProxyError::new(ErrorKind::Unauthorized, "Invalid management session"))?;
    request.extensions_mut().insert(session);
    Ok(next.run(request).await)
}

pub(super) async fn signup(
    State(state): State<ManagementState>,
    body: Result<Json<SignupRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ManagedAccount>), ProxyError> {
    let body = read_json(body)?;
    let email = credentials(&body.email, &body.password)?;
    let label = body.label.trim();
    if label.is_empty() || label.len() > 256 {
        return Err(ProxyError::new(
            ErrorKind::InvalidRequest,
            "Label must contain 1 to 256 bytes",
        ));
    }
    if body.signup_code.len() != 64
        || !body
            .signup_code
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ProxyError::new(
            ErrorKind::Unauthorized,
            "Invalid signup code",
        ));
    }
    let permit = state
        .password_jobs
        .clone()
        .try_acquire_owned()
        .map_err(|_| ProxyError::new(ErrorKind::Unavailable, "Password authentication is busy"))?;
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|_| ProxyError::new(ErrorKind::Unavailable, "Cannot hash password"))
    })
    .await
    .map_err(|_| ProxyError::new(ErrorKind::Unavailable, "Password worker failed"))??;
    let account = state
        .store
        .create_account(
            &email,
            &hash,
            label,
            &Sha256::digest(body.signup_code.as_bytes()),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(account)))
}

pub(super) async fn login(
    State(state): State<ManagementState>,
    body: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, ProxyError> {
    let body = read_json(body)?;
    let email = credentials(&body.email, &body.password)?;
    let account = state.store.password_account(&email).await?;
    let id = account.as_ref().map(|account| account.id);
    let permit = state
        .password_jobs
        .clone()
        .try_acquire_owned()
        .map_err(|_| ProxyError::new(ErrorKind::Unavailable, "Password authentication is busy"))?;
    let password = body.password;
    let valid = tokio::task::spawn_blocking(move || -> Result<bool, ProxyError> {
        let _permit = permit;
        let argon2 = Argon2::default();
        if let Some(account) = account {
            let hash = PasswordHash::new(&account.password_hash).map_err(|_| {
                ProxyError::new(ErrorKind::Persistence, "Invalid stored password hash")
            })?;
            Ok(argon2.verify_password(password.as_bytes(), &hash).is_ok())
        } else {
            // Missing accounts still perform the same bounded password work.
            argon2
                .hash_password(password.as_bytes())
                .map_err(|_| ProxyError::new(ErrorKind::Unavailable, "Cannot verify password"))?;
            Ok(false)
        }
    })
    .await
    .map_err(|_| ProxyError::new(ErrorKind::Unavailable, "Password worker failed"))??;
    let id = id
        .filter(|_| valid)
        .ok_or_else(|| ProxyError::new(ErrorKind::Unauthorized, "Invalid email or password"))?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at = Utc::now() + Duration::hours(24);
    let account = state
        .store
        .create_management_session(id, &Sha256::digest(token.as_bytes()), expires_at)
        .await?;
    let cookie = format!(
        "llmproxy_session={token}; Path=/management; HttpOnly; SameSite=Strict; Max-Age=86400{}",
        if state.cookie_secure { "; Secure" } else { "" }
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(LoginResponse {
            account,
            expires_at,
        }),
    )
        .into_response())
}

pub(super) async fn logout(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
) -> Result<Response, ProxyError> {
    state.store.revoke_management_session(&session).await?;
    let cookie = format!(
        "llmproxy_session=; Path=/management; HttpOnly; SameSite=Strict; Max-Age=0{}",
        if state.cookie_secure { "; Secure" } else { "" }
    );
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response())
}
