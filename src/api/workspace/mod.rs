mod postgres;

pub use postgres::PostgresWorkspaceStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkspace {
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspace {
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace not found")]
    NotFound,
    #[error("slug already in use: {0}")]
    DuplicateSlug(String),
    #[error("invalid workspace: {0}")]
    Invalid(String),
    #[error("internal store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait WorkspaceStore: Send + Sync {
    async fn list(&self) -> Result<Vec<Workspace>, WorkspaceError>;
    async fn get(&self, id: Uuid) -> Result<Workspace, WorkspaceError>;
    async fn create(&self, input: CreateWorkspace) -> Result<Workspace, WorkspaceError>;
    /// The slug stays: it is how the workspace is named in URLs and tokens.
    async fn rename(&self, id: Uuid, name: &str) -> Result<Workspace, WorkspaceError>;
    async fn delete(&self, id: Uuid) -> Result<(), WorkspaceError>;
}

pub(super) fn validate(input: &CreateWorkspace) -> Result<(), WorkspaceError> {
    if input.name.trim().is_empty() {
        return Err(WorkspaceError::Invalid("name must not be empty".into()));
    }
    if input.slug.trim().is_empty() {
        return Err(WorkspaceError::Invalid("slug must not be empty".into()));
    }
    if !input
        .slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(WorkspaceError::Invalid(
            "slug may contain only lowercase letters, digits and hyphens".into(),
        ));
    }
    Ok(())
}
