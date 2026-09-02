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
