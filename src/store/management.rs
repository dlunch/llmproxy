use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::PgStore;
use crate::core::{ErrorKind, ProviderKind, ProxyError, RoutingStrategy};

#[derive(Serialize, sqlx::FromRow)]
pub struct ManagedAccount {
    pub id: Uuid,
    pub email: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct PasswordAccount {
    pub id: Uuid,
    pub password_hash: String,
}

#[derive(Clone, sqlx::FromRow)]
pub struct ManagementSession {
    pub id: Uuid,
    pub account_id: Uuid,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ManagedApiToken {
    pub id: Uuid,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ManagedProvider {
    pub id: String,
    pub kind: ProviderKind,
    pub label: String,
    pub enabled: bool,
    pub authenticated: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfiguration {
    pub strategy: RoutingStrategy,
    pub routes: Vec<RouteConfiguration>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfiguration {
    pub model: String,
    pub provider_id: String,
    pub upstream_model: String,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TokenMetrics {
    pub token_id: Uuid,
    pub request_count: i64,
    pub running_requests: i64,
    pub succeeded_requests: i64,
    pub failed_requests: i64,
    pub cancelled_requests: i64,
    pub timed_out_requests: i64,
    pub interrupted_requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub missing_usage_requests: i64,
    pub mean_execution_latency_ms: Option<f64>,
    pub last_request_at: Option<DateTime<Utc>>,
}

fn write_error(error: sqlx::Error) -> ProxyError {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        ProxyError::new(ErrorKind::Conflict, "Resource already exists")
    } else {
        error.into()
    }
}

impl PgStore {
    pub async fn create_account(
        &self,
        email: &str,
        password_hash: &str,
        label: &str,
        signup_digest: &[u8],
    ) -> Result<ManagedAccount, ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let redeemed = sqlx::query("DELETE FROM signup_codes WHERE code_hash=$1")
            .bind(signup_digest)
            .execute(&mut *transaction)
            .await?;
        if redeemed.rows_affected() == 0 {
            return Err(ProxyError::new(
                ErrorKind::Unauthorized,
                "Invalid signup code",
            ));
        }
        let account = sqlx::query_as("INSERT INTO accounts(id,email,password_hash,label) VALUES ($1,$2,$3,$4) RETURNING id,email,label,created_at")
            .bind(Uuid::new_v4()).bind(email).bind(password_hash).bind(label)
            .fetch_one(&mut *transaction).await.map_err(write_error)?;
        transaction.commit().await?;
        Ok(account)
    }

    pub(crate) async fn password_account(
        &self,
        email: &str,
    ) -> Result<Option<PasswordAccount>, ProxyError> {
        Ok(sqlx::query_as("SELECT id,password_hash FROM accounts WHERE email=$1 AND enabled AND password_hash IS NOT NULL")
            .bind(email).fetch_optional(&self.pool).await?)
    }

    pub(crate) async fn create_management_session(
        &self,
        account_id: Uuid,
        token_digest: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<ManagedAccount, ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let account = sqlx::query_as("SELECT id,email,label,created_at FROM accounts WHERE id=$1 AND enabled AND email IS NOT NULL FOR SHARE")
            .bind(account_id).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::Unauthorized, "Invalid email or password"))?;
        sqlx::query("INSERT INTO management_sessions(id,account_id,token_hash,expires_at) VALUES ($1,$2,$3,$4)")
            .bind(Uuid::new_v4()).bind(account_id).bind(token_digest).bind(expires_at)
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(account)
    }

    pub(crate) async fn authenticate_management_session(
        &self,
        token_digest: &[u8],
    ) -> Result<Option<ManagementSession>, ProxyError> {
        Ok(sqlx::query_as("SELECT s.id,s.account_id FROM management_sessions s JOIN accounts a ON a.id=s.account_id WHERE s.token_hash=$1 AND s.expires_at>now() AND a.enabled AND a.email IS NOT NULL")
            .bind(token_digest).fetch_optional(&self.pool).await?)
    }

    pub(crate) async fn revoke_management_session(
        &self,
        session: &ManagementSession,
    ) -> Result<(), ProxyError> {
        sqlx::query("DELETE FROM management_sessions WHERE id=$1 AND account_id=$2")
            .bind(session.id)
            .bind(session.account_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn managed_account(&self, account_id: Uuid) -> Result<ManagedAccount, ProxyError> {
        sqlx::query_as(
            "SELECT id,email,label,created_at FROM accounts WHERE id=$1 AND email IS NOT NULL",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| ProxyError::new(ErrorKind::NotFound, "Account not found"))
    }

    pub async fn update_account_label(
        &self,
        account_id: Uuid,
        label: &str,
    ) -> Result<ManagedAccount, ProxyError> {
        sqlx::query_as("UPDATE accounts SET label=$2 WHERE id=$1 AND email IS NOT NULL RETURNING id,email,label,created_at")
            .bind(account_id).bind(label).fetch_optional(&self.pool).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::NotFound, "Account not found"))
    }

    pub async fn api_tokens(&self, account_id: Uuid) -> Result<Vec<ManagedApiToken>, ProxyError> {
        Ok(sqlx::query_as("SELECT id,label,created_at,revoked_at FROM api_tokens WHERE account_id=$1 ORDER BY created_at,id")
            .bind(account_id).fetch_all(&self.pool).await?)
    }

    pub async fn create_api_token(
        &self,
        account_id: Uuid,
        label: &str,
        digest: &[u8],
    ) -> Result<ManagedApiToken, ProxyError> {
        Ok(sqlx::query_as("INSERT INTO api_tokens(id,account_id,label,token_hash) VALUES ($1,$2,$3,$4) RETURNING id,label,created_at,revoked_at")
            .bind(Uuid::new_v4()).bind(account_id).bind(label).bind(digest)
            .fetch_one(&self.pool).await?)
    }

    pub async fn revoke_api_token(&self, account_id: Uuid, id: Uuid) -> Result<(), ProxyError> {
        let result = sqlx::query("UPDATE api_tokens SET revoked_at=COALESCE(revoked_at,now()) WHERE id=$1 AND account_id=$2")
            .bind(id).bind(account_id).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(ProxyError::new(ErrorKind::NotFound, "API token not found"));
        }
        Ok(())
    }

    pub async fn token_metrics(
        &self,
        account_id: Uuid,
        id: Uuid,
    ) -> Result<TokenMetrics, ProxyError> {
        sqlx::query_as("SELECT t.id AS token_id,count(r.id) AS request_count,\
            count(r.id) FILTER (WHERE r.outcome='running') AS running_requests,\
            count(r.id) FILTER (WHERE r.outcome='succeeded') AS succeeded_requests,\
            count(r.id) FILTER (WHERE r.outcome='failed') AS failed_requests,\
            count(r.id) FILTER (WHERE r.outcome='cancelled') AS cancelled_requests,\
            count(r.id) FILTER (WHERE r.outcome='timed_out') AS timed_out_requests,\
            count(r.id) FILTER (WHERE r.outcome='interrupted') AS interrupted_requests,\
            COALESCE(sum(r.input_tokens),0)::bigint AS input_tokens,COALESCE(sum(r.output_tokens),0)::bigint AS output_tokens,\
            count(r.id) FILTER (WHERE r.input_tokens IS NULL OR r.output_tokens IS NULL) AS missing_usage_requests,\
            avg(extract(epoch FROM (r.finished_at-r.started_at))*1000)::double precision AS mean_execution_latency_ms,\
            max(r.started_at) AS last_request_at \
            FROM api_tokens t LEFT JOIN requests r ON r.api_token_id=t.id AND r.account_id=t.account_id \
            WHERE t.id=$1 AND t.account_id=$2 GROUP BY t.id")
            .bind(id).bind(account_id).fetch_optional(&self.pool).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::NotFound, "API token not found"))
    }

    pub async fn managed_providers(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<ManagedProvider>, ProxyError> {
        Ok(sqlx::query_as("SELECT p.id,p.kind,p.label,p.enabled,CASE p.kind WHEN 'codex' THEN EXISTS(SELECT 1 FROM provider_credentials c WHERE c.provider_id=p.id) WHEN 'claude' THEN EXISTS(SELECT 1 FROM claude_credentials c WHERE c.provider_id=p.id) END AS authenticated FROM provider_instances p WHERE p.account_id=$1 AND p.kind IS NOT NULL ORDER BY p.label,p.id")
            .bind(account_id).fetch_all(&self.pool).await?)
    }

    pub async fn owned_provider(
        &self,
        account_id: Uuid,
        id: &str,
    ) -> Result<ManagedProvider, ProxyError> {
        sqlx::query_as("SELECT p.id,p.kind,p.label,p.enabled,CASE p.kind WHEN 'codex' THEN EXISTS(SELECT 1 FROM provider_credentials c WHERE c.provider_id=p.id) WHEN 'claude' THEN EXISTS(SELECT 1 FROM claude_credentials c WHERE c.provider_id=p.id) END AS authenticated FROM provider_instances p WHERE p.id=$1 AND p.account_id=$2 AND p.kind IS NOT NULL")
            .bind(id).bind(account_id).fetch_optional(&self.pool).await?
            .ok_or_else(|| ProxyError::new(ErrorKind::NotFound, "Provider not found"))
    }

    pub async fn register_provider(
        &self,
        account_id: Uuid,
        kind: ProviderKind,
        label: &str,
        enabled: bool,
    ) -> Result<ManagedProvider, ProxyError> {
        Ok(sqlx::query_as("INSERT INTO provider_instances(id,account_id,kind,label,enabled) VALUES ($1,$2,$3,$4,$5) RETURNING id,kind,label,enabled,false AS authenticated")
            .bind(Uuid::new_v4().to_string()).bind(account_id).bind(kind).bind(label).bind(enabled)
            .fetch_one(&self.pool).await?)
    }

    pub async fn update_provider(
        &self,
        account_id: Uuid,
        id: &str,
        label: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<ManagedProvider, ProxyError> {
        let result = sqlx::query("UPDATE provider_instances SET label=COALESCE($3,label),enabled=COALESCE($4,enabled) WHERE id=$1 AND account_id=$2 AND kind IS NOT NULL")
            .bind(id).bind(account_id).bind(label).bind(enabled).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(ProxyError::new(ErrorKind::NotFound, "Provider not found"));
        }
        self.owned_provider(account_id, id).await
    }

    pub async fn routing_configuration(
        &self,
        account_id: Uuid,
    ) -> Result<RoutingConfiguration, ProxyError> {
        let rows = sqlx::query("SELECT a.routing_strategy,m.model,m.provider_id,m.upstream_model,m.priority FROM accounts a LEFT JOIN (provider_models m JOIN provider_instances p ON p.id=m.provider_id) ON p.account_id=a.id WHERE a.id=$1 ORDER BY m.model,m.provider_id")
            .bind(account_id).fetch_all(&self.pool).await?;
        let strategy = rows
            .first()
            .ok_or_else(|| ProxyError::new(ErrorKind::NotFound, "Account not found"))?
            .try_get("routing_strategy")?;
        let mut routes = Vec::new();
        for row in rows {
            if let Some(model) = row.try_get::<Option<String>, _>("model")? {
                routes.push(RouteConfiguration {
                    model,
                    provider_id: row.try_get("provider_id")?,
                    upstream_model: row.try_get("upstream_model")?,
                    priority: row.try_get("priority")?,
                });
            }
        }
        Ok(RoutingConfiguration { strategy, routes })
    }

    pub async fn replace_routing(
        &self,
        account_id: Uuid,
        configuration: &RoutingConfiguration,
    ) -> Result<(), ProxyError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE accounts SET routing_strategy=$2 WHERE id=$1")
            .bind(account_id)
            .bind(configuration.strategy)
            .execute(&mut *transaction)
            .await?;
        let provider_ids = configuration
            .routes
            .iter()
            .map(|route| route.provider_id.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM provider_instances WHERE account_id=$1 AND id=ANY($2) AND kind IS NOT NULL")
            .bind(account_id).bind(&provider_ids).fetch_one(&mut *transaction).await?;
        if owned != provider_ids.len() as i64 {
            return Err(ProxyError::new(ErrorKind::NotFound, "Provider not found"));
        }
        sqlx::query("DELETE FROM provider_models m USING provider_instances p WHERE m.provider_id=p.id AND p.account_id=$1")
            .bind(account_id).execute(&mut *transaction).await?;
        if !configuration.routes.is_empty() {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO provider_models(provider_id,model,upstream_model,priority) ",
            );
            query.push_values(&configuration.routes, |mut row, route| {
                row.push_bind(&route.provider_id)
                    .push_bind(&route.model)
                    .push_bind(&route.upstream_model)
                    .push_bind(route.priority);
            });
            query
                .build()
                .execute(&mut *transaction)
                .await
                .map_err(write_error)?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn cleanup_expired_sessions(&self) -> Result<(), ProxyError> {
        sqlx::query("DELETE FROM management_sessions WHERE expires_at<=now()")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM provider_login_sessions WHERE id IN (SELECT id FROM provider_login_sessions WHERE expires_at<=now() FOR UPDATE SKIP LOCKED)")
            .execute(&self.pool).await?;
        Ok(())
    }
}
