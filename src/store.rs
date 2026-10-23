use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;

pub mod management;
pub(crate) mod oauth;

use crate::core::{
    AccountUsage, Credential, ErrorKind, Principal, ProviderKind, ProxyError, Route,
    RoutingContext, Usage,
};

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

#[derive(sqlx::FromRow)]
pub struct CodexCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub login_generation: i64,
}

#[derive(sqlx::FromRow)]
pub struct ClaudeCredentials {
    pub credentials: serde_json::Value,
    pub oauth_account: Option<serde_json::Value>,
    pub user_id: Option<String>,
    pub has_completed_onboarding: bool,
    pub updated_at: DateTime<Utc>,
    pub login_generation: i64,
}

pub struct QuotaWindow {
    pub key: String,
    pub observed_at: DateTime<Utc>,
    pub remaining_fraction: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

pub struct ProviderState {
    pub provider_id: String,
    pub accumulated_tokens: u64,
    pub quota: Vec<QuotaWindow>,
}

pub struct NewRequest<'a> {
    pub id: Uuid,
    pub account_id: Uuid,
    pub token_id: Option<Uuid>,
    pub ip_range_id: Option<Uuid>,
    pub client_ip: Option<std::net::IpAddr>,
    pub requested_model: &'a str,
    pub provider_id: &'a str,
    pub upstream_model: &'a str,
    pub started_at: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
}

impl From<sqlx::Error> for ProxyError {
    fn from(_: sqlx::Error) -> Self {
        Self::new(ErrorKind::Persistence, "Database operation failed")
    }
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), ProxyError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|_| ProxyError::new(ErrorKind::Persistence, "Database migration failed"))
    }

    pub async fn authenticate_token(&self, digest: &[u8]) -> Result<Option<Principal>, ProxyError> {
        let row: Option<(Uuid, Uuid)> = sqlx::query_as("SELECT t.id,t.account_id FROM api_tokens t JOIN accounts a ON a.id=t.account_id WHERE t.token_hash=$1 AND t.revoked_at IS NULL AND a.enabled")
            .bind(digest).fetch_optional(&self.pool).await?;
        Ok(row.map(|(id, account_id)| Principal {
            account_id,
            credential: Credential::ApiToken(id),
        }))
    }

    pub async fn authenticate_ip(
        &self,
        address: std::net::IpAddr,
    ) -> Result<Option<Principal>, ProxyError> {
        let row: Option<(Uuid, Uuid)> = sqlx::query_as("SELECT r.id,r.account_id FROM account_ip_ranges r JOIN accounts a ON a.id=r.account_id WHERE a.enabled AND r.network >>= $1::inet ORDER BY masklen(r.network) DESC,r.id LIMIT 1")
            .bind(ipnetwork::IpNetwork::from(address)).fetch_optional(&self.pool).await?;
        Ok(row.map(|(range_id, account_id)| Principal {
            account_id,
            credential: Credential::AllowedIp { range_id, address },
        }))
    }

    pub async fn begin_request(&self, request: NewRequest<'_>) -> Result<(), ProxyError> {
        sqlx::query("INSERT INTO requests (id,api_token_id,requested_model,provider_id,upstream_model,started_at,deadline_at,client_ip,account_id,ip_range_id,outcome) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'running')")
            .bind(request.id).bind(request.token_id).bind(request.requested_model)
            .bind(request.provider_id).bind(request.upstream_model)
            .bind(request.started_at).bind(request.deadline_at).bind(request.client_ip.map(ipnetwork::IpNetwork::from)).bind(request.account_id).bind(request.ip_range_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn finish_request(
        &self,
        id: Uuid,
        outcome: &str,
        error: Option<ErrorKind>,
        usage: Option<&Usage>,
    ) -> Result<(), ProxyError> {
        let counts = usage
            .map(|usage| {
                Ok::<_, ProxyError>((
                    i64::try_from(usage.input_tokens).map_err(|_| {
                        ProxyError::new(ErrorKind::Upstream, "Invalid upstream token count")
                    })?,
                    i64::try_from(usage.output_tokens).map_err(|_| {
                        ProxyError::new(ErrorKind::Upstream, "Invalid upstream token count")
                    })?,
                ))
            })
            .transpose()?;
        let result = sqlx::query("UPDATE requests SET outcome=$2,error_kind=$3,input_tokens=$4,output_tokens=$5,finished_at=now() WHERE id=$1 AND outcome='running'")
            .bind(id).bind(outcome).bind(error.map(ErrorKind::as_str))
            .bind(counts.map(|c| c.0)).bind(counts.map(|c| c.1))
            .execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(ProxyError::new(
                ErrorKind::Persistence,
                "Request was already finalized",
            ));
        }
        Ok(())
    }

    pub async fn routing_snapshot(
        &self,
        account_id: Uuid,
        model: &str,
    ) -> Result<(RoutingContext, Vec<Route>), ProxyError> {
        let rows = sqlx::query("SELECT a.input_tokens,a.output_tokens,a.routing_strategy,m.model,m.provider_id,m.upstream_model,m.priority,p.kind FROM accounts a LEFT JOIN (provider_instances p JOIN provider_models m ON m.provider_id=p.id AND m.model=$2) ON p.account_id=a.id AND p.enabled AND p.kind IS NOT NULL WHERE a.id=$1 AND a.enabled ORDER BY m.provider_id")
            .bind(account_id).bind(model).fetch_all(&self.pool).await?;
        let first = rows
            .first()
            .ok_or_else(|| ProxyError::new(ErrorKind::Unauthorized, "Account unavailable"))?;
        let context = RoutingContext {
            usage: AccountUsage {
                account_id,
                input_tokens: first.try_get::<i64, _>("input_tokens")? as u64,
                output_tokens: first.try_get::<i64, _>("output_tokens")? as u64,
            },
            strategy: first.try_get("routing_strategy")?,
        };
        let mut routes = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(provider_id) = row.try_get::<Option<String>, _>("provider_id")? {
                routes.push(Route {
                    model: row.try_get("model")?,
                    provider_id,
                    provider_kind: row.try_get("kind")?,
                    upstream_model: row.try_get("upstream_model")?,
                    priority: row.try_get("priority")?,
                });
            }
        }
        Ok((context, routes))
    }

    pub async fn provider_states(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<ProviderState>, ProxyError> {
        let rows = sqlx::query("SELECT p.id,p.input_tokens + p.output_tokens AS accumulated_tokens,q.window_key,q.observed_at,q.remaining_fraction,q.resets_at FROM provider_instances p LEFT JOIN provider_quota q ON q.provider_id=p.id AND q.login_generation=p.login_generation WHERE p.account_id=$1 AND p.enabled AND p.kind IS NOT NULL ORDER BY p.id")
            .bind(account_id).fetch_all(&self.pool).await?;
        let mut states = Vec::<ProviderState>::new();
        for row in rows {
            let provider: &str = row.try_get("id")?;
            if states
                .last()
                .is_none_or(|state| state.provider_id != provider)
            {
                states.push(ProviderState {
                    provider_id: provider.to_owned(),
                    accumulated_tokens: row.try_get::<i64, _>("accumulated_tokens")? as u64,
                    quota: Vec::new(),
                });
            }
            if let Some(key) = row.try_get::<Option<String>, _>("window_key")? {
                states
                    .last_mut()
                    .expect("provider row was initialized")
                    .quota
                    .push(QuotaWindow {
                        key,
                        observed_at: row.try_get("observed_at")?,
                        remaining_fraction: row.try_get("remaining_fraction")?,
                        resets_at: row.try_get("resets_at")?,
                    });
            }
        }
        Ok(states)
    }

    pub async fn enabled_providers(&self) -> Result<Vec<(String, ProviderKind)>, ProxyError> {
        Ok(sqlx::query_as("SELECT p.id,p.kind FROM provider_instances p JOIN accounts a ON a.id=p.account_id WHERE p.enabled AND a.enabled AND p.kind IS NOT NULL ORDER BY p.id")
            .fetch_all(&self.pool).await?)
    }

    pub async fn codex_credentials(
        &self,
        provider_id: &str,
    ) -> Result<CodexCredentials, ProxyError> {
        sqlx::query_as("SELECT c.access_token,c.refresh_token,c.account_id,c.expires_at,c.updated_at,p.login_generation FROM provider_credentials c JOIN provider_instances p ON p.id=c.provider_id WHERE c.provider_id=$1")
            .bind(provider_id).fetch_optional(&self.pool).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::UpstreamAuth, "Codex credentials unavailable"))
    }

    pub async fn lock_codex_credentials(
        &self,
        provider_id: &str,
    ) -> Result<(Transaction<'static, Postgres>, CodexCredentials), ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT access_token,refresh_token,account_id,expires_at,updated_at FROM provider_credentials WHERE provider_id=$1 FOR UPDATE")
            .bind(provider_id).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::UpstreamAuth, "Codex credentials unavailable"))?;
        // Read after locking: a joined locking SELECT can mix a new credential with an old generation.
        let login_generation =
            sqlx::query_scalar("SELECT login_generation FROM provider_instances WHERE id=$1")
                .bind(provider_id)
                .fetch_one(&mut *transaction)
                .await?;
        let credentials = CodexCredentials {
            access_token: row.try_get("access_token")?,
            refresh_token: row.try_get("refresh_token")?,
            account_id: row.try_get("account_id")?,
            expires_at: row.try_get("expires_at")?,
            updated_at: row.try_get("updated_at")?,
            login_generation,
        };
        Ok((transaction, credentials))
    }

    pub async fn try_lock_codex_credentials(
        &self,
        provider_id: &str,
    ) -> Result<Option<(Transaction<'static, Postgres>, CodexCredentials)>, ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT c.access_token,c.refresh_token,c.account_id,c.expires_at,c.updated_at FROM provider_credentials c JOIN provider_instances p ON p.id=c.provider_id JOIN accounts a ON a.id=p.account_id WHERE c.provider_id=$1 AND p.enabled AND a.enabled AND c.refresh_token<>'' FOR UPDATE OF c SKIP LOCKED")
            .bind(provider_id).fetch_optional(&mut *transaction).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let login_generation =
            sqlx::query_scalar("SELECT login_generation FROM provider_instances WHERE id=$1")
                .bind(provider_id)
                .fetch_one(&mut *transaction)
                .await?;
        let credentials = CodexCredentials {
            access_token: row.try_get("access_token")?,
            refresh_token: row.try_get("refresh_token")?,
            account_id: row.try_get("account_id")?,
            expires_at: row.try_get("expires_at")?,
            updated_at: row.try_get("updated_at")?,
            login_generation,
        };
        Ok(Some((transaction, credentials)))
    }

    pub async fn save_codex_credentials(
        mut transaction: Transaction<'static, Postgres>,
        provider_id: &str,
        credentials: &CodexCredentials,
    ) -> Result<(), ProxyError> {
        sqlx::query("UPDATE provider_credentials SET access_token=$2,refresh_token=$3,account_id=$4,expires_at=$5,updated_at=now() WHERE provider_id=$1")
            .bind(provider_id).bind(&credentials.access_token).bind(&credentials.refresh_token)
            .bind(&credentials.account_id).bind(credentials.expires_at).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn claude_credentials(
        &self,
        provider_id: &str,
    ) -> Result<ClaudeCredentials, ProxyError> {
        sqlx::query_as("SELECT c.credentials,c.oauth_account,c.user_id,c.has_completed_onboarding,c.updated_at,p.login_generation FROM claude_credentials c JOIN provider_instances p ON p.id=c.provider_id WHERE c.provider_id=$1")
            .bind(provider_id).fetch_optional(&self.pool).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::UpstreamAuth, "Claude subscription credentials are not configured"))
    }

    pub async fn lock_claude_credentials(
        &self,
        provider_id: &str,
    ) -> Result<(Transaction<'static, Postgres>, ClaudeCredentials), ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT credentials,oauth_account,user_id,has_completed_onboarding,updated_at FROM claude_credentials WHERE provider_id=$1 FOR UPDATE")
            .bind(provider_id).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::UpstreamAuth, "Claude subscription credentials are not configured"))?;
        let login_generation =
            sqlx::query_scalar("SELECT login_generation FROM provider_instances WHERE id=$1")
                .bind(provider_id)
                .fetch_one(&mut *transaction)
                .await?;
        let credentials = ClaudeCredentials {
            credentials: row.try_get("credentials")?,
            oauth_account: row.try_get("oauth_account")?,
            user_id: row.try_get("user_id")?,
            has_completed_onboarding: row.try_get("has_completed_onboarding")?,
            updated_at: row.try_get("updated_at")?,
            login_generation,
        };
        Ok((transaction, credentials))
    }

    pub async fn try_lock_claude_credentials(
        &self,
        provider_id: &str,
    ) -> Result<Option<(Transaction<'static, Postgres>, ClaudeCredentials)>, ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT c.credentials,c.oauth_account,c.user_id,c.has_completed_onboarding,c.updated_at FROM claude_credentials c JOIN provider_instances p ON p.id=c.provider_id JOIN accounts a ON a.id=p.account_id WHERE c.provider_id=$1 AND p.enabled AND a.enabled AND NULLIF(c.credentials #>> '{claudeAiOauth,refreshToken}','') IS NOT NULL FOR UPDATE OF c SKIP LOCKED")
            .bind(provider_id).fetch_optional(&mut *transaction).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let login_generation =
            sqlx::query_scalar("SELECT login_generation FROM provider_instances WHERE id=$1")
                .bind(provider_id)
                .fetch_one(&mut *transaction)
                .await?;
        let credentials = ClaudeCredentials {
            credentials: row.try_get("credentials")?,
            oauth_account: row.try_get("oauth_account")?,
            user_id: row.try_get("user_id")?,
            has_completed_onboarding: row.try_get("has_completed_onboarding")?,
            updated_at: row.try_get("updated_at")?,
            login_generation,
        };
        Ok(Some((transaction, credentials)))
    }

    pub async fn save_claude_credentials(
        mut transaction: Transaction<'static, Postgres>,
        provider_id: &str,
        credentials: &ClaudeCredentials,
    ) -> Result<(), ProxyError> {
        sqlx::query("UPDATE claude_credentials SET credentials=$2,oauth_account=$3,user_id=$4,has_completed_onboarding=$5,updated_at=CASE WHEN credentials #> '{claudeAiOauth,accessToken}' IS DISTINCT FROM $2::jsonb #> '{claudeAiOauth,accessToken}' OR credentials #> '{claudeAiOauth,refreshToken}' IS DISTINCT FROM $2::jsonb #> '{claudeAiOauth,refreshToken}' OR credentials #> '{claudeAiOauth,expiresAt}' IS DISTINCT FROM $2::jsonb #> '{claudeAiOauth,expiresAt}' THEN now() ELSE updated_at END WHERE provider_id=$1")
            .bind(provider_id).bind(&credentials.credentials).bind(&credentials.oauth_account)
            .bind(&credentials.user_id).bind(credentials.has_completed_onboarding).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn save_quota(
        &self,
        provider_id: &str,
        login_generation: i64,
        windows: &[QuotaWindow],
    ) -> Result<(), ProxyError> {
        if windows.is_empty() {
            return Ok(());
        }
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO provider_quota(provider_id,login_generation,window_key,observed_at,remaining_fraction,resets_at) ",
        );
        let mut ordered: Vec<_> = windows.iter().collect();
        ordered.sort_unstable_by(|left, right| left.key.cmp(&right.key));
        query.push_values(ordered, |mut row, window| {
            row.push_bind(provider_id)
                .push_bind(login_generation)
                .push_bind(&window.key)
                .push_bind(window.observed_at)
                .push_bind(window.remaining_fraction)
                .push_bind(window.resets_at);
        });
        query.push(" ON CONFLICT(provider_id,window_key) DO UPDATE SET login_generation=EXCLUDED.login_generation,observed_at=EXCLUDED.observed_at,remaining_fraction=EXCLUDED.remaining_fraction,resets_at=EXCLUDED.resets_at WHERE provider_quota.login_generation < EXCLUDED.login_generation OR (provider_quota.login_generation=EXCLUDED.login_generation AND provider_quota.observed_at <= EXCLUDED.observed_at)");
        query.build().execute(&self.pool).await?;
        Ok(())
    }

    pub async fn reconcile_expired_requests(&self) -> Result<u64, ProxyError> {
        Ok(sqlx::query("UPDATE requests SET outcome='interrupted',error_kind='timeout',finished_at=now() WHERE outcome='running' AND deadline_at < now() - interval '30 seconds'")
            .execute(&self.pool).await?.rows_affected())
    }
}
