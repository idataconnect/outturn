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
    pub role: String,
    pub content: String,
    /// What the agent did to produce this: `{"tool_calls": [...]}`, empty
    /// when it simply answered.
    pub metadata: serde_json::Value,
    /// How many `chat.delta` fragments this content already accounts for.
    ///
    /// A reply is created empty and streamed into, so while a turn is running
    /// the content is assembled from deltas rather than stored. This is where
    /// the next delta belongs, which is what lets a client resume a stream it
    /// reconnected to -- and refuse a fragment it has already folded in.
    pub delta_next: i32,
    pub model: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
}

/// A session's transcript, with the event cursor it was read at.
///
/// The two come from one statement, so they share a snapshot: every event at
/// or below `cursor` is already reflected in `messages`, and everything above
/// it is still to come over the feed. Polling from here is what stops history
/// and stream from overlapping -- the bug that appended a message's own text
/// to itself on reload.
#[derive(Debug, Clone, Serialize)]
pub struct History {
    pub messages: Vec<Message>,
    pub cursor: Uuid,
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

    async fn messages(&self, session_id: Uuid) -> Result<History, ChatError>;

    /// Replaces a message's content, for a reply that was created empty and
    /// streamed into.
    async fn set_message_content(
        &self,
        message_id: Uuid,
        content: &str,
        model: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<Message, ChatError>;

    /// Removes a message. Used to clear a placeholder whose turn failed, which
    /// would otherwise sit empty in the transcript forever.
    async fn delete_message(&self, message_id: Uuid) -> Result<(), ChatError>;

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
