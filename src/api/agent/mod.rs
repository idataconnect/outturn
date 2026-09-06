mod postgres;

pub use postgres::PostgresAgentStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub system_prompt: String,
    pub policy: serde_json::Value,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateAgent {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    /// How this agent runs: which model, which traffic type, how much the
    /// model deliberates, how many tool rounds a turn may take.
    ///
    /// Settable at creation because everything in it affects the very first
    /// turn. Without it an agent is created with an empty policy and the
    /// deployment's defaults -- which for a local model means thinking left
    /// on, and a reply that spends thirty seconds deliberating before its
    /// first visible character.
    #[serde(default)]
    pub policy: Option<serde_json::Value>,
}

/// Fields omitted are left unchanged, so a partial edit cannot silently blank
/// out what the caller did not send.
#[derive(Debug, Deserialize)]
pub struct UpdateAgent {
    pub name: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub policy: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent not found")]
    NotFound,
    #[error("slug already in use: {0}")]
    DuplicateSlug(String),
    #[error("invalid agent: {0}")]
    Invalid(String),
    #[error("agent store error: {0}")]
    Internal(String),
}

/// Every method takes the tenant explicitly: it comes from the caller's token,
/// and passing it on each call keeps a tenant-scoped predicate in every query
/// rather than relying on callers to remember to filter.
#[async_trait]
pub trait AgentStore: Send + Sync {
    async fn list(&self, tenant_id: Uuid) -> Result<Vec<Agent>, AgentError>;
    async fn get(&self, tenant_id: Uuid, id: Uuid) -> Result<Agent, AgentError>;
    async fn create(&self, tenant_id: Uuid, input: CreateAgent) -> Result<Agent, AgentError>;
    async fn update(
        &self,
        tenant_id: Uuid,
        id: Uuid,
        input: UpdateAgent,
    ) -> Result<Agent, AgentError>;
    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), AgentError>;
}

pub(super) fn validate_name(name: &str) -> Result<(), AgentError> {
    if name.trim().is_empty() {
        return Err(AgentError::Invalid("name must not be empty".into()));
    }
    Ok(())
}

pub(super) fn validate_slug(slug: &str) -> Result<(), AgentError> {
    if slug.trim().is_empty() {
        return Err(AgentError::Invalid("slug must not be empty".into()));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AgentError::Invalid(
            "slug may contain only lowercase letters, digits and hyphens".into(),
        ));
    }
    Ok(())
}
