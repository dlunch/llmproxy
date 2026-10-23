use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::{
    core::{ErrorKind, ProviderKind, ProxyError},
    providers::{
        claude::ClaudeAuth,
        codex::{CodexAuth, LoginPoll},
    },
    store::{
        PgStore,
        management::ManagementSession,
        oauth::{LoginCredentials, ProviderLogin},
    },
};

use super::ManagementState;

pub struct OAuthManager {
    store: PgStore,
    codex: CodexAuth,
    claude: ClaudeAuth,
    tasks: TaskTracker,
}

#[derive(Serialize)]
struct LoginStatus {
    id: Uuid,
    provider_id: String,
    status: String,
    expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_poll_at: Option<DateTime<Utc>>,
}

impl From<&ProviderLogin> for LoginStatus {
    fn from(login: &ProviderLogin) -> Self {
        let status = if matches!(login.status.as_str(), "pending" | "processing")
            && login.expires_at <= Utc::now()
        {
            "expired".to_owned()
        } else {
            login.status.clone()
        };
        Self {
            id: login.id,
            provider_id: login.provider_id.clone(),
            status,
            expires_at: login.expires_at,
            interval_seconds: login.interval_seconds,
            next_poll_at: login.next_poll_at,
        }
    }
}

#[derive(Serialize)]
struct StartedLogin {
    #[serde(flatten)]
    login: LoginStatus,
    authorization_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_code: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteLogin {
    code: Option<String>,
}

pub(super) fn router() -> Router<ManagementState> {
    Router::new()
        .route("/providers/{id}/logins", post(start))
        .route("/providers/{id}/logins/{login_id}", get(status))
        .route("/providers/{id}/logins/{login_id}/complete", post(complete))
}

impl OAuthManager {
    pub fn new(store: PgStore, codex: CodexAuth, claude: ClaudeAuth) -> Self {
        Self {
            store,
            codex,
            claude,
            tasks: TaskTracker::new(),
        }
    }

    pub async fn shutdown(&self) {
        self.tasks.close();
        self.tasks.wait().await;
    }

    async fn start(
        &self,
        account_id: Uuid,
        provider_id: String,
    ) -> Result<StartedLogin, ProxyError> {
        let provider = self.store.owned_provider(account_id, &provider_id).await?;
        let mut login = ProviderLogin {
            id: Uuid::new_v4(),
            provider_id,
            kind: provider.kind,
            status: "pending".into(),
            expires_at: Utc::now(),
            device_auth_id: None,
            user_code: None,
            interval_seconds: None,
            next_poll_at: None,
            state: None,
            code_verifier: None,
        };
        let authorization_url = match provider.kind {
            ProviderKind::Codex => {
                let device = self.codex.start_device_login().await?;
                login.device_auth_id = Some(device.device_auth_id);
                login.user_code = Some(device.user_code);
                login.interval_seconds = Some(device.interval_seconds);
                login.next_poll_at = Some(Utc::now());
                login.expires_at = device.expires_at;
                device.verification_url
            }
            ProviderKind::Claude => {
                let browser = self.claude.start_login();
                login.state = Some(browser.state);
                login.code_verifier = Some(browser.code_verifier);
                login.expires_at = browser.expires_at;
                browser.authorization_url
            }
        };
        self.store
            .replace_provider_login(account_id, &login)
            .await?;
        Ok(StartedLogin {
            login: LoginStatus::from(&login),
            authorization_url,
            user_code: login.user_code,
        })
    }

    async fn complete(
        self: &Arc<Self>,
        account_id: Uuid,
        provider_id: String,
        id: Uuid,
        request: CompleteLogin,
    ) -> Result<(StatusCode, Json<LoginStatus>), ProxyError> {
        let login = self
            .store
            .provider_login(account_id, &provider_id, id)
            .await?;
        if login.status == "completed" {
            return Ok((StatusCode::OK, Json(LoginStatus::from(&login))));
        }
        if login.expires_at <= Utc::now() || login.status == "failed" {
            return Err(ProxyError::new(
                ErrorKind::Conflict,
                "Provider login is no longer active",
            ));
        }
        if login.status == "processing" {
            return Ok((StatusCode::ACCEPTED, Json(LoginStatus::from(&login))));
        }
        let code = match login.kind {
            ProviderKind::Codex => {
                if request.code.is_some() {
                    return Err(ProxyError::new(
                        ErrorKind::InvalidRequest,
                        "Codex device login does not accept a code",
                    ));
                }
                None
            }
            ProviderKind::Claude => {
                let (code, _) = request
                    .code
                    .as_deref()
                    .map(str::trim)
                    .filter(|input| input.len() <= 8192)
                    .and_then(|input| input.split_once('#'))
                    .filter(|(code, state)| {
                        !code.is_empty() && Some(*state) == login.state.as_deref()
                    })
                    .ok_or_else(|| {
                        ProxyError::new(
                            ErrorKind::InvalidRequest,
                            "Submit the full code from this provider login",
                        )
                    })?;
                Some(code.to_owned())
            }
        };
        if !self
            .store
            .claim_provider_login(account_id, &provider_id, id)
            .await?
        {
            let current = self
                .store
                .provider_login(account_id, &provider_id, id)
                .await?;
            if current.status == "failed"
                || current.expires_at <= Utc::now() && current.status != "completed"
            {
                return Err(ProxyError::new(
                    ErrorKind::Conflict,
                    "Provider login is no longer active",
                ));
            }
            let status = if current.status == "completed" {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            };
            return Ok((status, Json(LoginStatus::from(&current))));
        }
        let manager = self.clone();
        let operation = self
            .tasks
            .spawn(async move { manager.exchange(account_id, provider_id, id, code).await });
        let login = operation.await.map_err(|_| {
            ProxyError::new(ErrorKind::UpstreamAuth, "Provider login task failed")
        })??;
        let status = if login.status == "completed" {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        };
        Ok((status, Json(login)))
    }

    async fn exchange(
        &self,
        account_id: Uuid,
        provider_id: String,
        id: Uuid,
        code: Option<String>,
    ) -> Result<LoginStatus, ProxyError> {
        let (transaction, mut login) = self
            .store
            .lock_provider_login(account_id, &provider_id, id)
            .await?;
        if login.expires_at <= Utc::now() {
            PgStore::fail_provider_login(transaction, &login).await?;
            return Err(ProxyError::new(
                ErrorKind::Conflict,
                "Provider login expired before exchange",
            ));
        }
        let result = match login.kind {
            ProviderKind::Codex => self
                .codex
                .poll_device_login(
                    login
                        .device_auth_id
                        .as_deref()
                        .expect("pending Codex login has a device ID"),
                    login
                        .user_code
                        .as_deref()
                        .expect("pending Codex login has a user code"),
                )
                .await
                .map(|poll| match poll {
                    LoginPoll::Pending => None,
                    LoginPoll::Complete(credentials) => Some(LoginCredentials::Codex(credentials)),
                }),
            ProviderKind::Claude => self
                .claude
                .complete_login(
                    code.as_deref()
                        .expect("Claude completion validated its code"),
                    login
                        .state
                        .as_deref()
                        .expect("pending Claude login has state"),
                    login
                        .code_verifier
                        .as_deref()
                        .expect("pending Claude login has a verifier"),
                )
                .await
                .map(|credentials| Some(LoginCredentials::Claude(credentials))),
        };
        match result {
            Ok(Some(credentials)) => {
                PgStore::finish_provider_login(transaction, &login, credentials).await?;
                login.status = "completed".into();
                login.interval_seconds = None;
                login.next_poll_at = None;
                Ok(LoginStatus::from(&login))
            }
            Ok(None) => {
                let next_poll = Utc::now()
                    + chrono::Duration::seconds(
                        login
                            .interval_seconds
                            .expect("pending Codex login has an interval"),
                    );
                PgStore::defer_provider_login(transaction, &login, next_poll).await?;
                login.status = "pending".into();
                login.next_poll_at = Some(next_poll);
                Ok(LoginStatus::from(&login))
            }
            Err(error) => {
                PgStore::fail_provider_login(transaction, &login).await?;
                Err(error)
            }
        }
    }
}

async fn start(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    Path(provider_id): Path<String>,
) -> Result<(StatusCode, Json<StartedLogin>), ProxyError> {
    state
        .oauth
        .start(session.account_id, provider_id)
        .await
        .map(|login| (StatusCode::CREATED, Json(login)))
}

async fn status(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    Path((provider_id, id)): Path<(String, Uuid)>,
) -> Result<Json<LoginStatus>, ProxyError> {
    let login = state
        .store
        .provider_login(session.account_id, &provider_id, id)
        .await?;
    Ok(Json(LoginStatus::from(&login)))
}

async fn complete(
    State(state): State<ManagementState>,
    Extension(session): Extension<ManagementSession>,
    Path((provider_id, id)): Path<(String, Uuid)>,
    request: Result<Json<CompleteLogin>, JsonRejection>,
) -> Result<(StatusCode, Json<LoginStatus>), ProxyError> {
    let request = super::read_json(request)?;
    state
        .oauth
        .complete(session.account_id, provider_id, id, request)
        .await
}
