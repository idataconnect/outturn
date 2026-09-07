mod postgres;

pub use postgres::PostgresUserStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Role;

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: Uuid,
    pub display_name: String,
    /// Every way this account can sign in.
    pub identities: Vec<Identity>,
    pub system_roles: Vec<Role>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub id: Uuid,
    pub provider: String,
    /// Email address for password and magic-link identities; the provider's
    /// subject id for OAuth.
    pub subject: String,
    pub verified: bool,
}

/// The password provider. Named rather than inlined so the string appears once.
pub const PROVIDER_PASSWORD: &str = "password";

/// A tenant the user may sign in to, with the roles they hold there.
#[derive(Debug, Clone, Serialize)]
pub struct TenantMembership {
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    /// Names of the tenant's roles this account holds there.
    pub roles: Vec<String>,
}

/// Creates an account together with its first password identity.
#[derive(Debug, Deserialize)]
pub struct CreateUser {
    pub email: String,
    pub display_name: String,
    pub password: String,
}

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error("user not found")]
    NotFound,
    #[error("email already in use: {0}")]
    DuplicateEmail(String),
    #[error("identity not found")]
    IdentityNotFound,
    #[error("invalid user: {0}")]
    Invalid(String),
    #[error("invalid credentials")]
    BadCredentials,
    #[error("internal store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait UserStore: Send + Sync {
    async fn list(&self) -> Result<Vec<User>, UserError>;
    /// Accounts holding a role in one tenant. What a tenant's administrator
    /// is shown: the accounts of other tenants are not theirs to see.
    async fn list_for_tenant(&self, tenant_id: Uuid) -> Result<Vec<User>, UserError>;
    async fn get(&self, id: Uuid) -> Result<User, UserError>;
    async fn rename(&self, id: Uuid, display_name: &str) -> Result<User, UserError>;
    async fn create(&self, input: CreateUser) -> Result<User, UserError>;
    async fn delete(&self, id: Uuid) -> Result<(), UserError>;

    /// Verifies the password and returns the user on success.
    async fn authenticate(&self, email: &str, password: &str) -> Result<User, UserError>;

    /// Tenants this user may sign in to. A system admin sees every tenant.
    async fn memberships(&self, user_id: Uuid) -> Result<Vec<TenantMembership>, UserError>;

    /// Names of the roles the user holds in one tenant, with their platform
    /// roles alongside. This is what a token carries.
    async fn roles_for_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Vec<String>, UserError>;

    /// Adds another way to sign in to an existing account.
    async fn add_password_identity(
        &self,
        user_id: Uuid,
        email: &str,
        password: &str,
    ) -> Result<Identity, UserError>;

    /// Removes one identity. Refuses to remove the last one, which would
    /// orphan the account.
    async fn remove_identity(&self, user_id: Uuid, identity_id: Uuid) -> Result<(), UserError>;

    async fn grant_system_role(&self, user_id: Uuid, role: Role) -> Result<(), UserError>;
    /// Grants one of the tenant's roles, by name. A name the tenant has no
    /// role for is refused rather than recorded.
    async fn grant_tenant_role(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        role: &str,
    ) -> Result<(), UserError>;
    async fn revoke_tenant_role(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        role: &str,
    ) -> Result<(), UserError>;

    /// True when no user exists yet — used to gate dev seeding.
    async fn is_empty(&self) -> Result<bool, UserError>;
}

pub(super) fn validate(input: &CreateUser) -> Result<(), UserError> {
    if !input.email.contains('@') || input.email.trim().is_empty() {
        return Err(UserError::Invalid("email must be a valid address".into()));
    }
    if input.display_name.trim().is_empty() {
        return Err(UserError::Invalid("display name must not be empty".into()));
    }
    if input.password.len() < 8 {
        return Err(UserError::Invalid(
            "password must be at least 8 characters".into(),
        ));
    }
    Ok(())
}
