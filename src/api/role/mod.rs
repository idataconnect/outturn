//! Roles a workspace defines for itself.
//!
//! A role is a name and a bundle of authorities, owned by one workspace. Every
//! workspace starts with copies of `rbac::DEFAULT_ROLES` and may edit them, add
//! to them, or replace them. Nothing about a workspace's roles is visible to or
//! shared with another workspace, and the platform's own roles are not here at
//! all -- see `rbac::Role` for those.
//!
//! Membership -- which roles a person holds -- is a separate question, and
//! today has one source, `user_workspace_roles`. It is kept apart from the roles
//! themselves so a second source can be added later (groups from an identity
//! provider, mapped onto local roles) without the meaning of a role moving
//! anywhere: authorities only ever come from a workspace's own rows.

mod postgres;

use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Authority;

pub use postgres::PostgresRoleStore;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceRole {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub description: String,
    /// As their wire names, sorted, so the browser can show and edit them.
    pub authorities: Vec<String>,
    /// How many people hold this role here. Shown so an editor knows what a
    /// change touches, and checked so a role in use is not deleted.
    pub holders: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateRole {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub authorities: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRole {
    pub name: Option<String>,
    pub description: Option<String>,
    pub authorities: Option<Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum RoleError {
    #[error("role not found")]
    NotFound,
    #[error("a role named {0} already exists here")]
    Duplicate(String),
    #[error("{0}")]
    Invalid(String),
    /// Somebody still holds it. Said separately so the browser can explain
    /// rather than report a generic failure.
    #[error("{0} people hold this role; take it away from them first")]
    InUse(i64),
    #[error("role store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait RoleStore: Send + Sync {
    async fn list(&self, workspace_id: Uuid) -> Result<Vec<WorkspaceRole>, RoleError>;
    async fn get(&self, workspace_id: Uuid, id: Uuid) -> Result<WorkspaceRole, RoleError>;
    async fn create(&self, workspace_id: Uuid, input: CreateRole) -> Result<WorkspaceRole, RoleError>;
    async fn update(&self, workspace_id: Uuid, id: Uuid, input: UpdateRole) -> Result<WorkspaceRole, RoleError>;
    async fn delete(&self, workspace_id: Uuid, id: Uuid) -> Result<(), RoleError>;

    /// The authorities that follow from holding these roles in this workspace.
    ///
    /// Called on every authorised request, so implementations cache per
    /// workspace and drop the entry when a role there changes. Names that match
    /// no role -- a platform role, or one deleted since the token was minted
    /// -- contribute nothing.
    async fn authorities_for(
        &self,
        workspace_id: Uuid,
        roles: &[String],
    ) -> Result<HashSet<Authority>, RoleError>;

    /// Copies the default roles into a workspace that has none yet.
    async fn seed_defaults(&self, workspace_id: Uuid) -> Result<(), RoleError>;
}

/// Checks a list of authority names a workspace wants in a role.
///
/// Refuses names that are not authorities and names that are reserved to the
/// platform. Returns them parsed and deduplicated.
pub fn validate_authorities(names: &[String]) -> Result<Vec<Authority>, RoleError> {
    let mut seen = HashSet::new();
    let mut parsed = Vec::with_capacity(names.len());
    for name in names {
        let authority = Authority::parse(name.trim())
            .ok_or_else(|| RoleError::Invalid(format!("{name} is not an authority")))?;
        if !authority.workspace_assignable() {
            return Err(RoleError::Invalid(format!(
                "{authority} is reserved to the platform and cannot be put in a role"
            )));
        }
        if seen.insert(authority) {
            parsed.push(authority);
        }
    }
    Ok(parsed)
}

/// Checks a role name a workspace wants to use.
pub fn validate_name(name: &str) -> Result<String, RoleError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RoleError::Invalid("a role needs a name".into()));
    }
    if name.len() > 64 {
        return Err(RoleError::Invalid("a role name can be at most 64 characters".into()));
    }
    if crate::auth::rbac::is_platform_role_name(name) {
        return Err(RoleError::Invalid(format!(
            "{name} is the name of a platform role and cannot be used here"
        )));
    }
    Ok(name.to_string())
}
