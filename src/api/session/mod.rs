mod postgres;

pub use postgres::PostgresSessionStore;

use async_trait::async_trait;
use uuid::Uuid;

/// How long a refresh token remains usable. Long enough that a person is not
/// asked to sign in during ordinary use; short enough that an abandoned
/// session does not live forever.
pub const REFRESH_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct RefreshSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub family_id: Uuid,
}

/// A newly issued refresh token: the secret is returned once, at creation,
/// and only its hash is kept.
#[derive(Debug, Clone)]
pub struct IssuedRefresh {
    pub token: String,
    pub session: RefreshSession,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The token does not exist, is expired, or was revoked.
    #[error("refresh token is not valid")]
    Invalid,
    /// The token was already rotated away, which means someone is replaying an
    /// old one. The family has been revoked.
    #[error("refresh token was replayed; session revoked")]
    Replayed,
    #[error("session store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Starts a new session family, at login.
    async fn issue(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        user_agent: Option<&str>,
    ) -> Result<IssuedRefresh, SessionError>;

    /// Exchanges a refresh token for a new one, retiring the presented token.
    ///
    /// Presenting an already-rotated token is treated as theft: the whole
    /// family is revoked and `Replayed` returned.
    async fn rotate(
        &self,
        token: &str,
        user_agent: Option<&str>,
    ) -> Result<IssuedRefresh, SessionError>;

    /// Ends one session (sign out).
    async fn revoke(&self, token: &str) -> Result<(), SessionError>;

    /// Ends every session for a user, e.g. after a password change.
    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<u64, SessionError>;

    /// Removes rows that are past use. Called periodically.
    async fn sweep_expired(&self) -> Result<u64, SessionError>;
}
