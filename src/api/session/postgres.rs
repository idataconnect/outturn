use std::time::Duration;

use async_trait::async_trait;
use rand::RngCore;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{IssuedRefresh, REFRESH_LIFETIME_SECS, RefreshSession, SessionError, SessionStore};

pub struct PostgresSessionStore {
    pool: PgPool,
}

impl PostgresSessionStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> SessionError {
    SessionError::Internal(e.to_string())
}

/// 256 bits of randomness, urlsafe-base64 encoded.
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Refresh tokens are high-entropy random strings rather than passwords, so a
/// fast hash is appropriate: there is nothing to brute-force. Argon2 here would
/// only make every refresh slow.
fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn insert(
    pool: &PgPool,
    user_id: Uuid,
    workspace_id: Uuid,
    family_id: Uuid,
    user_agent: Option<&str>,
) -> Result<IssuedRefresh, SessionError> {
    let token = generate_token();
    let id = Uuid::now_v7();

    sqlx::query(
        "insert into refresh_tokens \
             (id, user_id, workspace_id, token_hash, family_id, user_agent, expires_at) \
         values ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7))",
    )
    .bind(id)
    .bind(user_id)
    .bind(workspace_id)
    .bind(hash_token(&token))
    .bind(family_id)
    .bind(user_agent)
    .bind(Duration::from_secs(REFRESH_LIFETIME_SECS).as_secs_f64())
    .execute(pool)
    .await
    .map_err(internal)?;

    Ok(IssuedRefresh {
        token,
        session: RefreshSession {
            id,
            user_id,
            workspace_id,
            family_id,
        },
    })
}

#[async_trait]
impl SessionStore for PostgresSessionStore {
    async fn issue(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        user_agent: Option<&str>,
    ) -> Result<IssuedRefresh, SessionError> {
        // A login starts its own family, so revoking one compromised session
        // does not disturb the user's other devices.
        insert(&self.pool, user_id, workspace_id, Uuid::now_v7(), user_agent).await
    }

    async fn rotate(
        &self,
        token: &str,
        user_agent: Option<&str>,
    ) -> Result<IssuedRefresh, SessionError> {
        // Retired in the same statement that reads it. A select followed by
        // an update left a window in which two presentations of one token
        // both passed the replay check and both got successors -- so a
        // replay in that window went undetected. Now exactly one presenter
        // gets the row back; every other sees it already rotated.
        let retired = sqlx::query(
            "update refresh_tokens set rotated_at = now() \
             where token_hash = $1 and rotated_at is null \
             returning id, user_id, workspace_id, family_id, revoked_at, \
                       expires_at < now() as expired",
        )
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;

        let Some(row) = retired else {
            // Either no such token, or one that was already rotated. The
            // second is a replay: the legitimate holder or an attacker has
            // a stale copy, and there is no way to tell which, so the whole
            // family goes.
            let family: Option<Uuid> = sqlx::query_scalar(
                "select family_id from refresh_tokens where token_hash = $1",
            )
            .bind(hash_token(token))
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?;

            let Some(family_id) = family else {
                return Err(SessionError::Invalid);
            };
            sqlx::query(
                "update refresh_tokens set revoked_at = now() \
                 where family_id = $1 and revoked_at is null",
            )
            .bind(family_id)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
            tracing::warn!(
                family_id = %family_id,
                "refresh token replayed; revoking session family"
            );
            return Err(SessionError::Replayed);
        };

        let family_id: Uuid = row.get("family_id");
        if row
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("revoked_at")
            .is_some()
            || row.get::<bool, _>("expired")
        {
            return Err(SessionError::Invalid);
        }

        insert(
            &self.pool,
            row.get("user_id"),
            row.get("workspace_id"),
            family_id,
            user_agent,
        )
        .await
    }

    async fn revoke(&self, token: &str) -> Result<(), SessionError> {
        sqlx::query(
            "update refresh_tokens set revoked_at = now() \
             where token_hash = $1 and revoked_at is null",
        )
        .bind(hash_token(token))
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<u64, SessionError> {
        let result = sqlx::query(
            "update refresh_tokens set revoked_at = now() \
             where user_id = $1 and revoked_at is null",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(result.rows_affected())
    }

    async fn sweep_expired(&self) -> Result<u64, SessionError> {
        // Rotated and revoked rows are kept until expiry so a replay is still
        // detectable; only genuinely past-use rows are removed.
        let result = sqlx::query("delete from refresh_tokens where expires_at < now()")
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(result.rows_affected())
    }
}
