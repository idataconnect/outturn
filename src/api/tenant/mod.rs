mod postgres;

pub use postgres::PostgresTenantStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateTenant {
    pub name: String,
    pub slug: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TenantError {
    #[error("tenant not found")]
    NotFound,
    #[error("slug already in use: {0}")]
    DuplicateSlug(String),
    #[error("invalid tenant: {0}")]
    Invalid(String),
    #[error("internal store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait TenantStore: Send + Sync {
    async fn list(&self) -> Result<Vec<Tenant>, TenantError>;
    async fn get(&self, id: Uuid) -> Result<Tenant, TenantError>;
    async fn create(&self, input: CreateTenant) -> Result<Tenant, TenantError>;
    async fn delete(&self, id: Uuid) -> Result<(), TenantError>;
}


pub(super) fn validate(input: &CreateTenant) -> Result<(), TenantError> {
    if input.name.trim().is_empty() {
        return Err(TenantError::Invalid("name must not be empty".into()));
    }
    if input.slug.trim().is_empty() {
        return Err(TenantError::Invalid("slug must not be empty".into()));
    }
    if !input
        .slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(TenantError::Invalid(
            "slug may contain only lowercase letters, digits and hyphens".into(),
        ));
    }
    Ok(())
}
