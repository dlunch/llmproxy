use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::core::{AccountUsage, Credential, ErrorKind, Principal, ProxyError, Usage};

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
}

#[derive(sqlx::FromRow)]
pub struct ClaudeCredentials {
    pub credentials: serde_json::Value,
    pub oauth_account: Option<serde_json::Value>,
    pub user_id: Option<String>,
    pub has_completed_onboarding: bool,
    pub updated_at: DateTime<Utc>,
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

    pub async fn account_usage(&self, account_id: Uuid) -> Result<AccountUsage, ProxyError> {
        let (input, output): (i64, i64) = sqlx::query_as("SELECT COALESCE(SUM(input_tokens),0)::bigint,COALESCE(SUM(output_tokens),0)::bigint FROM requests WHERE account_id=$1")
            .bind(account_id).fetch_one(&self.pool).await?;
        Ok(AccountUsage {
            account_id,
            input_tokens: input as u64,
            output_tokens: output as u64,
        })
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

    pub async fn provider_routes(
        &self,
        account_id: Uuid,
        provider_ids: &[String],
    ) -> Result<Vec<crate::core::Route>, ProxyError> {
        let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT m.model,m.provider_id,m.upstream_model FROM provider_models m JOIN provider_instances p ON p.id=m.provider_id WHERE p.account_id=$1 AND p.id=ANY($2) ORDER BY m.model,m.provider_id")
            .bind(account_id).bind(provider_ids).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|(model, provider_id, upstream_model)| crate::core::Route {
                model,
                provider_id,
                upstream_model,
            })
            .collect())
    }

    pub async fn provider_states(
        &self,
        account_id: Uuid,
        provider_ids: &[String],
    ) -> Result<Vec<ProviderState>, ProxyError> {
        let usage: Vec<(String, i64)> = sqlx::query_as("SELECT p.id, COALESCE(SUM(COALESCE(r.input_tokens,0)::numeric + COALESCE(r.output_tokens,0)::numeric),0)::bigint FROM provider_instances p LEFT JOIN requests r ON r.provider_id=p.id AND r.account_id=p.account_id WHERE p.account_id=$1 AND p.id=ANY($2) GROUP BY p.id")
            .bind(account_id).bind(provider_ids).fetch_all(&self.pool).await?;
        let eligible_ids: Vec<_> = usage.iter().map(|(id, _)| id.as_str()).collect();
        let quotas = sqlx::query("SELECT provider_id,window_key,observed_at,remaining_fraction,resets_at FROM provider_quota WHERE provider_id=ANY($1)")
            .bind(&eligible_ids).fetch_all(&self.pool).await?;
        let mut states: Vec<_> = usage
            .into_iter()
            .map(|(provider_id, tokens)| ProviderState {
                provider_id,
                accumulated_tokens: tokens as u64,
                quota: vec![],
            })
            .collect();
        for row in quotas {
            let provider: String = row.try_get("provider_id")?;
            if let Some(state) = states.iter_mut().find(|s| s.provider_id == provider) {
                state.quota.push(QuotaWindow {
                    key: row.try_get("window_key")?,
                    observed_at: row.try_get("observed_at")?,
                    remaining_fraction: row.try_get("remaining_fraction")?,
                    resets_at: row.try_get("resets_at")?,
                });
            }
        }
        Ok(states)
    }

    pub async fn lock_codex_credentials(
        &self,
        provider_id: &str,
    ) -> Result<(Transaction<'static, Postgres>, CodexCredentials), ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let credentials = sqlx::query_as("SELECT access_token,refresh_token,account_id,expires_at,updated_at FROM provider_credentials WHERE provider_id=$1 FOR UPDATE")
            .bind(provider_id).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::UpstreamAuth, "Codex credentials unavailable"))?;
        Ok((transaction, credentials))
    }

    pub async fn try_lock_codex_credentials(
        &self,
        provider_id: &str,
    ) -> Result<Option<(Transaction<'static, Postgres>, CodexCredentials)>, ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let credentials = sqlx::query_as("SELECT c.access_token,c.refresh_token,c.account_id,c.expires_at,c.updated_at FROM provider_credentials c JOIN provider_instances p ON p.id=c.provider_id JOIN accounts a ON a.id=p.account_id WHERE c.provider_id=$1 AND a.enabled AND c.refresh_token<>'' FOR UPDATE OF c SKIP LOCKED")
            .bind(provider_id).fetch_optional(&mut *transaction).await?;
        Ok(credentials.map(|credentials| (transaction, credentials)))
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

    pub async fn lock_claude_credentials(
        &self,
        provider_id: &str,
    ) -> Result<(Transaction<'static, Postgres>, ClaudeCredentials), ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let credentials = sqlx::query_as("SELECT credentials,oauth_account,user_id,has_completed_onboarding,updated_at FROM claude_credentials WHERE provider_id=$1 FOR UPDATE")
            .bind(provider_id).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::UpstreamAuth, "Claude subscription credentials are not configured"))?;
        Ok((transaction, credentials))
    }

    pub async fn try_lock_claude_credentials(
        &self,
        provider_id: &str,
    ) -> Result<Option<(Transaction<'static, Postgres>, ClaudeCredentials)>, ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let credentials = sqlx::query_as("SELECT c.credentials,c.oauth_account,c.user_id,c.has_completed_onboarding,c.updated_at FROM claude_credentials c JOIN provider_instances p ON p.id=c.provider_id JOIN accounts a ON a.id=p.account_id WHERE c.provider_id=$1 AND a.enabled AND NULLIF(c.credentials #>> '{claudeAiOauth,refreshToken}','') IS NOT NULL FOR UPDATE OF c SKIP LOCKED")
            .bind(provider_id).fetch_optional(&mut *transaction).await?;
        Ok(credentials.map(|credentials| (transaction, credentials)))
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
        windows: &[QuotaWindow],
    ) -> Result<(), ProxyError> {
        let mut transaction = self.pool.begin().await?;
        for window in windows {
            sqlx::query("INSERT INTO provider_quota(provider_id,window_key,observed_at,remaining_fraction,resets_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT(provider_id,window_key) DO UPDATE SET observed_at=EXCLUDED.observed_at,remaining_fraction=EXCLUDED.remaining_fraction,resets_at=EXCLUDED.resets_at WHERE provider_quota.observed_at <= EXCLUDED.observed_at")
                .bind(provider_id).bind(&window.key).bind(window.observed_at)
                .bind(window.remaining_fraction).bind(window.resets_at).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn reconcile_expired_requests(&self) -> Result<u64, ProxyError> {
        Ok(sqlx::query("UPDATE requests SET outcome='interrupted',error_kind='timeout',finished_at=now() WHERE outcome='running' AND deadline_at < now() - interval '30 seconds'")
            .execute(&self.pool).await?.rows_affected())
    }
}
