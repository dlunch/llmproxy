use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::core::{ErrorKind, ProviderKind, ProxyError};

use super::{ClaudeCredentials, CodexCredentials, PgStore};

#[derive(sqlx::FromRow)]
pub(crate) struct ProviderLogin {
    pub id: Uuid,
    pub provider_id: String,
    pub kind: ProviderKind,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub device_auth_id: Option<String>,
    pub user_code: Option<String>,
    pub interval_seconds: Option<i64>,
    pub next_poll_at: Option<DateTime<Utc>>,
    pub state: Option<String>,
    pub code_verifier: Option<String>,
}

pub(crate) enum LoginCredentials {
    Codex(CodexCredentials),
    Claude(ClaudeCredentials),
}

impl PgStore {
    pub(crate) async fn replace_provider_login(
        &self,
        account_id: Uuid,
        login: &ProviderLogin,
    ) -> Result<(), ProxyError> {
        let result = sqlx::query("INSERT INTO provider_login_sessions(id,provider_id,status,expires_at,device_auth_id,user_code,interval_seconds,next_poll_at,state,code_verifier) SELECT $1,p.id,'pending',$4,$5,$6,$7,$8,$9,$10 FROM provider_instances p WHERE p.id=$2 AND p.account_id=$3 ON CONFLICT(provider_id) DO UPDATE SET id=EXCLUDED.id,status='pending',created_at=now(),expires_at=EXCLUDED.expires_at,device_auth_id=EXCLUDED.device_auth_id,user_code=EXCLUDED.user_code,interval_seconds=EXCLUDED.interval_seconds,next_poll_at=EXCLUDED.next_poll_at,state=EXCLUDED.state,code_verifier=EXCLUDED.code_verifier WHERE provider_login_sessions.status<>'processing' OR provider_login_sessions.expires_at<=now()")
            .bind(login.id).bind(&login.provider_id).bind(account_id).bind(login.expires_at)
            .bind(&login.device_auth_id).bind(&login.user_code).bind(login.interval_seconds)
            .bind(login.next_poll_at).bind(&login.state).bind(&login.code_verifier)
            .execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(ProxyError::new(
                ErrorKind::Conflict,
                "A provider login is already processing",
            ));
        }
        Ok(())
    }

    pub(crate) async fn provider_login(
        &self,
        account_id: Uuid,
        provider_id: &str,
        id: Uuid,
    ) -> Result<ProviderLogin, ProxyError> {
        sqlx::query_as("SELECT s.*,p.kind FROM provider_login_sessions s JOIN provider_instances p ON p.id=s.provider_id WHERE p.account_id=$1 AND s.provider_id=$2 AND s.id=$3")
            .bind(account_id).bind(provider_id).bind(id).fetch_optional(&self.pool).await?
            .ok_or_else(||ProxyError::new(ErrorKind::NotFound,"Provider login not found"))
    }

    pub(crate) async fn claim_provider_login(
        &self,
        account_id: Uuid,
        provider_id: &str,
        id: Uuid,
    ) -> Result<bool, ProxyError> {
        let result = sqlx::query("UPDATE provider_login_sessions s SET status='processing' FROM provider_instances p WHERE p.id=s.provider_id AND p.account_id=$1 AND s.provider_id=$2 AND s.id=$3 AND s.status='pending' AND s.expires_at>now() AND (s.next_poll_at IS NULL OR s.next_poll_at<=now())")
            .bind(account_id).bind(provider_id).bind(id).execute(&self.pool).await?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn lock_provider_login(
        &self,
        account_id: Uuid,
        provider_id: &str,
        id: Uuid,
    ) -> Result<(Transaction<'static, Postgres>, ProviderLogin), ProxyError> {
        let mut transaction = self.pool.begin().await?;
        let login: ProviderLogin = sqlx::query_as("SELECT s.*,p.kind FROM provider_login_sessions s JOIN provider_instances p ON p.id=s.provider_id WHERE p.account_id=$1 AND s.provider_id=$2 AND s.id=$3 AND s.status='processing' FOR UPDATE OF s")
            .bind(account_id).bind(provider_id).bind(id).fetch_optional(&mut *transaction).await?
            .ok_or_else(||ProxyError::new(ErrorKind::Conflict,"Provider login was replaced"))?;
        match login.kind {
            ProviderKind::Codex => {
                sqlx::query(
                    "SELECT provider_id FROM provider_credentials WHERE provider_id=$1 FOR UPDATE",
                )
                .bind(provider_id)
                .fetch_optional(&mut *transaction)
                .await?;
            }
            ProviderKind::Claude => {
                sqlx::query(
                    "SELECT provider_id FROM claude_credentials WHERE provider_id=$1 FOR UPDATE",
                )
                .bind(provider_id)
                .fetch_optional(&mut *transaction)
                .await?;
            }
        }
        Ok((transaction, login))
    }

    pub(crate) async fn defer_provider_login(
        mut transaction: Transaction<'static, Postgres>,
        login: &ProviderLogin,
        next_poll_at: DateTime<Utc>,
    ) -> Result<(), ProxyError> {
        sqlx::query(
            "UPDATE provider_login_sessions SET status='pending',next_poll_at=$2 WHERE id=$1",
        )
        .bind(login.id)
        .bind(next_poll_at)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub(crate) async fn fail_provider_login(
        mut transaction: Transaction<'static, Postgres>,
        login: &ProviderLogin,
    ) -> Result<(), ProxyError> {
        sqlx::query("UPDATE provider_login_sessions SET status='failed',device_auth_id=NULL,user_code=NULL,interval_seconds=NULL,next_poll_at=NULL,state=NULL,code_verifier=NULL WHERE id=$1")
            .bind(login.id).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub(crate) async fn finish_provider_login(
        mut transaction: Transaction<'static, Postgres>,
        login: &ProviderLogin,
        credentials: LoginCredentials,
    ) -> Result<(), ProxyError> {
        sqlx::query(
            "UPDATE provider_instances SET login_generation=login_generation+1 WHERE id=$1",
        )
        .bind(&login.provider_id)
        .execute(&mut *transaction)
        .await?;
        match credentials {
            LoginCredentials::Codex(credentials) => {
                sqlx::query("INSERT INTO provider_credentials(provider_id,access_token,refresh_token,account_id,expires_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(provider_id) DO UPDATE SET access_token=EXCLUDED.access_token,refresh_token=EXCLUDED.refresh_token,account_id=EXCLUDED.account_id,expires_at=EXCLUDED.expires_at,updated_at=now()")
                    .bind(&login.provider_id).bind(credentials.access_token).bind(credentials.refresh_token)
                    .bind(credentials.account_id).bind(credentials.expires_at).execute(&mut *transaction).await?;
            }
            LoginCredentials::Claude(credentials) => {
                sqlx::query("INSERT INTO claude_credentials(provider_id,credentials,oauth_account,user_id,has_completed_onboarding) VALUES($1,$2,$3,$4,$5) ON CONFLICT(provider_id) DO UPDATE SET credentials=EXCLUDED.credentials,oauth_account=EXCLUDED.oauth_account,user_id=EXCLUDED.user_id,has_completed_onboarding=EXCLUDED.has_completed_onboarding,updated_at=now()")
                    .bind(&login.provider_id).bind(credentials.credentials).bind(credentials.oauth_account)
                    .bind(credentials.user_id).bind(credentials.has_completed_onboarding).execute(&mut *transaction).await?;
            }
        }
        sqlx::query("UPDATE provider_login_sessions SET status='completed',device_auth_id=NULL,user_code=NULL,interval_seconds=NULL,next_poll_at=NULL,state=NULL,code_verifier=NULL WHERE id=$1")
            .bind(login.id).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }
}
