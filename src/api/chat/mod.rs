mod postgres;

pub use postgres::PostgresChatStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct AgentSession {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub agent_id: Uuid,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSession {
    pub agent_id: Uuid,
    #[serde(default)]
    pub title: String,
}

/// Token counts reported by a provider, recorded per message so usage can be
/// attributed to a tenant later.
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("session not found")]
    NotFound,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("chat store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait ChatStore: Send + Sync {
    async fn create_session(
        &self,
        tenant_id: Uuid,
        user_id: Uuid,
        input: CreateSession,
    ) -> Result<AgentSession, ChatError>;

    async fn list_sessions(&self, tenant_id: Uuid) -> Result<Vec<AgentSession>, ChatError>;

    async fn get_session(
        &self,
        tenant_id: Uuid,
        session_id: Uuid,
    ) -> Result<AgentSession, ChatError>;

    async fn delete_session(&self, tenant_id: Uuid, session_id: Uuid) -> Result<(), ChatError>;

    async fn messages(&self, session_id: Uuid) -> Result<Vec<Message>, ChatError>;

    /// Appends a message, assigning the next sequence number for the session.
    async fn append_message(
        &self,
        session_id: Uuid,
        role: &str,
        content: &str,
        model: Option<&str>,
        usage: Usage,
    ) -> Result<Message, ChatError>;
}
