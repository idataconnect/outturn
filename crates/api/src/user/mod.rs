mod postgres;

pub use postgres::PostgresUserStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use outturn_core::auth::Role;

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

/// A workspace the user may sign in to, with the roles they hold there.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceMembership {
    pub workspace_id: Uuid,
    pub name: String,
    pub slug: String,
    /// Names of the workspace's roles this account holds there.
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
    async fn list(&self, after: Option<Uuid>, limit: i64) -> Result<Vec<User>, UserError>;
    /// Accounts holding a role in one workspace. What a workspace's administrator
    /// is shown: the accounts of other workspaces are not theirs to see.
    async fn list_for_workspace(
        &self,
        workspace_id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<User>, UserError>;
    async fn get(&self, id: Uuid) -> Result<User, UserError>;
    /// `get`, for an account holding a role in `workspace_id`. An account that
    /// exists but belongs elsewhere is `NotFound`, the same as one that does
    /// not exist: the workspace is the isolation boundary, and a route that
    /// resolved by id alone let one workspace's administrator reach every
    /// account on the platform. Answering 403 for those would make the ids
    /// an oracle for who is where.
    async fn get_in_workspace(&self, id: Uuid, workspace_id: Uuid) -> Result<User, UserError>;
    async fn rename(&self, id: Uuid, display_name: &str) -> Result<User, UserError>;
    async fn create(&self, input: CreateUser) -> Result<User, UserError>;
    async fn delete(&self, id: Uuid) -> Result<(), UserError>;

    /// Verifies the password and returns the user on success.
    async fn authenticate(&self, email: &str, password: &str) -> Result<User, UserError>;

    /// Workspaces this user may sign in to. A system admin sees every workspace.
    async fn memberships(&self, user_id: Uuid) -> Result<Vec<WorkspaceMembership>, UserError>;

    /// Names of the roles the user holds in one workspace, with their platform
    /// roles alongside. This is what a token carries.
    async fn roles_for_workspace(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Vec<String>, UserError>;

    /// Adds another way to sign in to an existing account.
    async fn add_password_identity(
        &self,
        user_id: Uuid,
        email: &str,
        password: &str,
    ) -> Result<Identity, UserError>;

    /// Sets a new password on one of the account's password identities. With
    /// `current`, the old password must match it -- a person changing their
    /// own; without, it is somebody allowed to manage the account setting it.
    async fn set_password(
        &self,
        user_id: Uuid,
        identity_id: Uuid,
        current: Option<&str>,
        new: &str,
    ) -> Result<(), UserError>;

    /// Whether `password` is the password of any of the account's sign-ins:
    /// the proof a person gives before changing how their own account is
    /// signed in to.
    async fn verify_password(&self, user_id: Uuid, password: &str) -> Result<bool, UserError>;

    /// Removes one identity. Refuses to remove the last one, which would
    /// orphan the account.
    async fn remove_identity(&self, user_id: Uuid, identity_id: Uuid) -> Result<(), UserError>;

    async fn grant_system_role(&self, user_id: Uuid, role: Role) -> Result<(), UserError>;
    /// Grants one of the workspace's roles, by name. A name the workspace has no
    /// role for is refused rather than recorded.
    async fn grant_workspace_role(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        role: &str,
    ) -> Result<(), UserError>;
    async fn revoke_workspace_role(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        role: &str,
    ) -> Result<(), UserError>;

    /// True when no user exists yet — used to gate dev seeding.
    async fn is_empty(&self) -> Result<bool, UserError>;
}

/// Shortest password accepted, in bytes. The UI's `minLength` says the same.
pub const MIN_PASSWORD: usize = 8;

/// The rule every password meets, wherever it is set: creating an account,
/// adding a sign-in, or changing one.
pub(super) fn validate_password(password: &str) -> Result<(), UserError> {
    if password.len() < MIN_PASSWORD {
        return Err(UserError::Invalid(format!(
            "password must be at least {MIN_PASSWORD} characters"
        )));
    }
    Ok(())
}

pub(super) fn validate(input: &CreateUser) -> Result<(), UserError> {
    if !input.email.contains('@') || input.email.trim().is_empty() {
        return Err(UserError::Invalid("email must be a valid address".into()));
    }
    if input.display_name.trim().is_empty() {
        return Err(UserError::Invalid("display name must not be empty".into()));
    }
    validate_password(&input.password)?;
    Ok(())
}
