use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::{self, Authority, SessionClaims, TokenMinter, TokenValidator};

use super::workspace::{CreateWorkspace, Workspace, WorkspaceError, WorkspaceStore};
use super::agent::AgentStore;
use super::chat::ChatStore;
use super::session::SessionStore;
use super::role::{CreateRole, RoleError, RoleStore, WorkspaceRole, UpdateRole};
use super::user::{CreateUser, Identity, WorkspaceMembership, User, UserStore};

pub struct ApiState {
    pub(super) workspaces: Arc<dyn WorkspaceStore>,
    pub(super) users: Arc<dyn UserStore>,
    pub(super) sessions: Arc<dyn SessionStore>,
    pub(super) agents: Arc<dyn AgentStore>,
    /// Prose an agent is given beside its system prompt, and which of it each
    /// agent gets.
    pub(super) skills: Arc<dyn super::skill::SkillStore>,
    pub(super) chat: Arc<dyn ChatStore>,
    /// What a workspace's roles mean. Consulted on every authorised request.
    pub(super) roles: Arc<dyn RoleStore>,
    pub(super) usage: Arc<dyn super::usage::UsageStore>,
    pub(super) settings: Arc<dyn super::settings::SettingsStore>,
    /// The same bucket the runtime reads and writes, so a file a person
    /// uploads is one the agent can name. Absent when none is configured.
    pub(super) storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
    pub(super) auth: TokenValidator,
    pub(super) minter: TokenMinter,
    /// What the runtime tier presents. Not a token: see `RuntimeKey`.
    pub(super) runtime_key: crate::auth::RuntimeKey,
    pub(super) pool: sqlx::PgPool,
    pub(super) bus: crate::events::EventBus,
    /// Fires on shutdown so parked long polls return instead of holding the
    /// drain open for their full timeout.
    pub(super) shutdown: Arc<tokio::sync::Notify>,
    /// Prepares turns and records what they produce. Set after construction,
    /// because the worker holds this state and the two would otherwise have to
    /// be built at once.
    pub(super) worker: std::sync::OnceLock<Arc<super::worker::Worker>>,
}

impl ApiState {
    /// Gives this state the worker that serves turns to runtimes.
    pub fn set_worker(&self, worker: Arc<super::worker::Worker>) {
        let _ = self.worker.set(worker);
    }
}

impl ApiState {
    pub fn new(
        workspaces: Arc<dyn WorkspaceStore>,
        users: Arc<dyn UserStore>,
        sessions: Arc<dyn SessionStore>,
        agents: Arc<dyn AgentStore>,
        skills: Arc<dyn super::skill::SkillStore>,
        chat: Arc<dyn ChatStore>,
        roles: Arc<dyn RoleStore>,
        usage: Arc<dyn super::usage::UsageStore>,
        settings: Arc<dyn super::settings::SettingsStore>,
        storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
        auth: TokenValidator,
        minter: TokenMinter,
        runtime_key: crate::auth::RuntimeKey,
        pool: sqlx::PgPool,
        bus: crate::events::EventBus,
        shutdown: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            workspaces,
            users,
            sessions,
            agents,
            skills,
            chat,
            roles,
            usage,
            settings,
            storage,
            auth,
            minter,
            runtime_key,
            pool,
            bus,
            shutdown,
            worker: std::sync::OnceLock::new(),
        }
    }
}

pub(super) type ApiError = (StatusCode, String);

pub(super) fn authenticate(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
) -> Result<SessionClaims, ApiError> {
    // Browsers send the HttpOnly session cookie; service-to-service callers
    // send a bearer token. The cookie is preferred so a stale Authorization
    // header cannot shadow a fresh session.
    if let Some(token) = auth::session_from_cookies(headers) {
        return state
            .auth
            .validate(token)
            .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()));
    }

    let bearer = auth::extract_bearer(headers)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "not authenticated".to_string()))?;

    // The runtime's shared key is accepted only here, on the bearer path. It
    // is not a token and is never parsed as one; a match means exactly one
    // thing, which is what the claims it maps to say.
    if state.runtime_key.accepts(bearer) {
        return Ok(auth::RuntimeKey::claims());
    }

    state
        .auth
        .validate(bearer)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))
}

/// Everything the caller may do, resolved for this request.
///
/// Platform roles resolve in code; workspace roles resolve through the role
/// store, which caches per workspace. Done per request rather than at mint so an
/// edit to a role takes effect on the next click, not at the next refresh.
pub(super) async fn authorities_of(
    state: &ApiState,
    claims: &SessionClaims,
) -> Result<std::collections::HashSet<Authority>, ApiError> {
    let mut granted = auth::platform_authorities(&claims.roles);
    granted.extend(
        state
            .roles
            .authorities_for(claims.workspace_id, &claims.roles)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    );
    Ok(granted)
}

pub(super) async fn require(
    state: &ApiState,
    claims: &SessionClaims,
    authority: Authority,
) -> Result<(), ApiError> {
    if authorities_of(state, claims).await?.contains(&authority) {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, auth::AuthError::Forbidden.to_string()))
    }
}

pub(super) async fn authorize(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
    authority: Authority,
) -> Result<SessionClaims, ApiError> {
    let claims = authenticate(state, headers)?;
    require(state, &claims, authority).await?;
    Ok(claims)
}

impl From<RoleError> for ApiError {
    fn from(e: RoleError) -> Self {
        let status = match e {
            RoleError::NotFound => StatusCode::NOT_FOUND,
            RoleError::Duplicate(_) | RoleError::InUse(_) => StatusCode::CONFLICT,
            RoleError::Invalid(_) => StatusCode::BAD_REQUEST,
            RoleError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

impl From<super::egress::RuleError> for ApiError {
    fn from(e: super::egress::RuleError) -> Self {
        use super::egress::RuleError;
        match e {
            // The message is written for whoever typed the rule, so it is the
            // response body rather than something only a log sees.
            RuleError::Invalid(m) => (StatusCode::BAD_REQUEST, m),
            RuleError::Duplicate(m) => (StatusCode::CONFLICT, m),
            RuleError::Database(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        }
    }
}

impl From<super::skill::SkillError> for ApiError {
    fn from(e: super::skill::SkillError) -> Self {
        use super::skill::SkillError;
        let status = match e {
            SkillError::NotFound => StatusCode::NOT_FOUND,
            SkillError::DuplicateSlug(_) => StatusCode::CONFLICT,
            SkillError::Invalid(_) => StatusCode::BAD_REQUEST,
            // Not forbidden to this caller so much as not yet allowed to
            // anyone here: the workspace has not opened those hosts.
            SkillError::HostsNotAllowed(_) => StatusCode::CONFLICT,
            SkillError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

impl From<WorkspaceError> for ApiError {
    fn from(e: WorkspaceError) -> Self {
        let status = match e {
            WorkspaceError::NotFound => StatusCode::NOT_FOUND,
            WorkspaceError::DuplicateSlug(_) => StatusCode::CONFLICT,
            WorkspaceError::Invalid(_) => StatusCode::BAD_REQUEST,
            WorkspaceError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

/// Everything the UI needs to restore a session on a cold load.
///
/// Deliberately includes the display name and memberships, not just ids: a
/// reload has only this endpoint to work from, and a client left to stitch the
/// identity together from several calls renders a signed-in user as nameless
/// and workspaceless until they all land -- which is precisely what it did.
#[derive(Debug, Serialize)]
struct SessionInfo {
    session_id: Uuid,
    workspace_id: Uuid,
    display_name: String,
    roles: Vec<String>,
    authorities: Vec<String>,
    workspaces: Vec<WorkspaceMembership>,
}

async fn session_info(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<SessionInfo>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let mut authorities: Vec<String> = authorities_of(&state, &claims)
        .await?
        .iter()
        .map(|a| a.as_str().to_string())
        .collect();
    authorities.sort();

    // The session id is the account id, which is what the login response is
    // built from too.
    let user = state.users.get(claims.subject).await?;
    let workspaces = state.users.memberships(claims.subject).await?;

    Ok(Json(SessionInfo {
        session_id: claims.subject,
        workspace_id: claims.workspace_id,
        display_name: user.display_name,
        roles: claims.roles.clone(),
        authorities,
        workspaces,
    }))
}

async fn list_workspaces(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Workspace>>, ApiError> {
    authorize(&state, &headers, Authority::WorkspacesRead).await?;
    Ok(Json(state.workspaces.list().await?))
}

async fn create_workspace(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateWorkspace>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let claims = authorize(&state, &headers, Authority::WorkspacesCreate).await?;
    let workspace = state.workspaces.create(input).await?;
    // A workspace with no roles is one nobody can be given access to.
    state.roles.seed_defaults(workspace.id).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %workspace.id,
        slug = %workspace.slug,
        "workspace created"
    );
    Ok((StatusCode::CREATED, Json(workspace)))
}

async fn get_workspace(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Workspace>, ApiError> {
    authorize(&state, &headers, Authority::WorkspacesRead).await?;
    Ok(Json(state.workspaces.get(id).await?))
}

async fn update_workspace(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<super::workspace::UpdateWorkspace>,
) -> Result<Json<Workspace>, ApiError> {
    let claims = authorize(&state, &headers, Authority::WorkspacesUpdate).await?;
    let workspace = state.workspaces.rename(id, &input.name).await?;
    tracing::info!(actor = %claims.subject, workspace_id = %id, "workspace renamed");
    Ok(Json(workspace))
}

async fn delete_workspace(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::WorkspacesDelete).await?;
    state.workspaces.delete(id).await?;
    tracing::info!(actor = %claims.subject, workspace_id = %id, "workspace deleted");
    Ok(StatusCode::NO_CONTENT)
}


#[derive(Debug, serde::Deserialize)]
struct GrantRole {
    /// The name of one of the workspace's roles.
    role: String,
}

/// The hosts this workspace's agents may reach.
///
/// Scoped to the caller's own workspace throughout, taken from the token rather
/// than from a path: an egress list is the shape of what a workspace's agents can
/// reach, and reading somebody else's tells you where to aim an injection.
async fn list_egress_rules(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<super::egress::Rule>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsRead).await?;
    Ok(Json(super::egress::list(&state.pool, claims.workspace_id).await?))
}

async fn create_egress_rule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<super::egress::CreateRule>,
) -> Result<(StatusCode, Json<super::egress::Rule>), ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsUpdate).await?;
    let rule = super::egress::create(&state.pool, claims.workspace_id, input).await?;
    // Worth a line in the log on its own: this is the moment a workspace's agents
    // gained somewhere new to send things.
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        host = %rule.host,
        "egress rule added"
    );
    Ok((StatusCode::CREATED, Json(rule)))
}

async fn delete_egress_rule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsUpdate).await?;
    if !super::egress::delete(&state.pool, claims.workspace_id, id).await? {
        return Err((StatusCode::NOT_FOUND, "no such rule".into()));
    }
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        rule_id = %id,
        "egress rule removed"
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn list_users(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<User>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::UsersRead).await?;
    // A workspace's administrator sees the accounts in their workspace. Every
    // account on the platform is a system administrator's view alone: the
    // workspace is the isolation boundary, and a user list that crossed it
    // named every other customer's staff.
    if claims.is_system_admin() {
        Ok(Json(state.users.list().await?))
    } else {
        Ok(Json(state.users.list_for_workspace(claims.workspace_id).await?))
    }
}

/// A user, with where they belong.
///
/// Memberships are narrowed to what the caller may know about: their own
/// workspace, or every workspace for a system administrator.
#[derive(Debug, serde::Serialize)]
struct UserDetail {
    #[serde(flatten)]
    user: User,
    memberships: Vec<WorkspaceMembership>,
}

#[derive(Debug, serde::Deserialize)]
struct UpdateUser {
    display_name: String,
}

async fn update_user(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateUser>,
) -> Result<Json<User>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    if claims.subject != id {
        require(&state, &claims, Authority::UsersUpdate).await?;
    }
    let user = state.users.rename(id, &input.display_name).await?;
    tracing::info!(actor = %claims.subject, user_id = %id, "user renamed");
    Ok(Json(user))
}

/// A new account, and optionally a role for it in the creator's workspace.
///
/// The role rides in the same request because the user list is scoped to the
/// workspace: an account created without a role there is one its creator can no
/// longer see, and two requests left a window for exactly that.
#[derive(Debug, serde::Deserialize)]
struct CreateUserRequest {
    #[serde(flatten)]
    user: CreateUser,
    #[serde(default)]
    role: Option<String>,
}

async fn create_user(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<User>), ApiError> {
    let claims = authorize(&state, &headers, Authority::UsersCreate).await?;
    let CreateUserRequest { user: input, role } = input;
    if role.is_some() {
        require(&state, &claims, Authority::RolesAssign).await?;
    }
    let user = state.users.create(input).await?;
    if let Some(role) = role {
        state
            .users
            .grant_workspace_role(user.id, claims.workspace_id, &role)
            .await?;
    }
    tracing::info!(actor = %claims.subject, user_id = %user.id, "user created");
    Ok((StatusCode::CREATED, Json(user)))
}

async fn get_user(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<UserDetail>, ApiError> {
    let claims = authorize(&state, &headers, Authority::UsersRead).await?;
    let user = state.users.get(id).await?;
    let mut memberships = state.users.memberships(id).await?;
    if !claims.is_system_admin() {
        memberships.retain(|m| m.workspace_id == claims.workspace_id);
    }
    Ok(Json(UserDetail { user, memberships }))
}

async fn delete_user(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::UsersDelete).await?;
    if claims.subject == id {
        return Err((StatusCode::BAD_REQUEST, "cannot delete yourself".into()));
    }
    state.users.delete(id).await?;
    tracing::info!(actor = %claims.subject, user_id = %id, "user deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, serde::Deserialize)]
struct AddIdentity {
    email: String,
    password: String,
}

async fn add_identity(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(input): Json<AddIdentity>,
) -> Result<(StatusCode, Json<Identity>), ApiError> {
    // Users may add sign-in methods to their own account; managing anyone
    // else's needs the user-management authority.
    let claims = authenticate(&state, &headers)?;
    if claims.subject != user_id {
        require(&state, &claims, Authority::UsersUpdate).await?;
    }

    let identity = state
        .users
        .add_password_identity(user_id, &input.email, &input.password)
        .await?;
    tracing::info!(actor = %claims.subject, user_id = %user_id, "identity added");
    Ok((StatusCode::CREATED, Json(identity)))
}

async fn remove_identity(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((user_id, identity_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    if claims.subject != user_id {
        require(&state, &claims, Authority::UsersUpdate).await?;
    }

    state.users.remove_identity(user_id, identity_id).await?;
    tracing::info!(actor = %claims.subject, user_id = %user_id, "identity removed");
    Ok(StatusCode::NO_CONTENT)
}

async fn grant_workspace_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((user_id, workspace_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<GrantRole>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesAssign).await?;
    // A workspace admin may only grant within their own workspace; a system admin
    // carries the authority in whichever workspace they are scoped to.
    if claims.workspace_id != workspace_id && !claims.is_system_admin() {
        return Err((StatusCode::FORBIDDEN, "cannot grant outside your workspace".into()));
    }
    state
        .users
        .grant_workspace_role(user_id, workspace_id, &input.role)
        .await?;
    tracing::info!(
        actor = %claims.subject,
        user_id = %user_id,
        workspace_id = %workspace_id,
        role = %input.role,
        "workspace role granted"
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_workspace_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((user_id, workspace_id, role)): Path<(Uuid, Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesAssign).await?;
    if claims.workspace_id != workspace_id && !claims.is_system_admin() {
        return Err((StatusCode::FORBIDDEN, "cannot revoke outside your workspace".into()));
    }
    state
        .users
        .revoke_workspace_role(user_id, workspace_id, &role)
        .await?;
    tracing::info!(
        actor = %claims.subject,
        user_id = %user_id,
        workspace_id = %workspace_id,
        role = %role,
        "workspace role revoked"
    );
    Ok(StatusCode::NO_CONTENT)
}


// -- Roles --------------------------------------------------------------------

/// The vocabulary a role can be built from, for the role editor.
#[derive(Debug, serde::Serialize)]
struct AuthorityInfo {
    name: &'static str,
    description: &'static str,
}

async fn list_authorities(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<AuthorityInfo>>, ApiError> {
    authenticate(&state, &headers)?;
    Ok(Json(
        Authority::ALL
            .iter()
            .filter(|a| a.workspace_assignable())
            .map(|a| AuthorityInfo {
                name: a.as_str(),
                description: a.describe(),
            })
            .collect(),
    ))
}

/// Seeing the roles takes either the authority to hand them out or the
/// authority to define them: both jobs need the list.
async fn list_roles(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<WorkspaceRole>>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let granted = authorities_of(&state, &claims).await?;
    if !granted.contains(&Authority::RolesAssign) && !granted.contains(&Authority::RolesManage) {
        return Err((StatusCode::FORBIDDEN, auth::AuthError::Forbidden.to_string()));
    }
    Ok(Json(state.roles.list(claims.workspace_id).await?))
}

async fn get_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkspaceRole>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let granted = authorities_of(&state, &claims).await?;
    if !granted.contains(&Authority::RolesAssign) && !granted.contains(&Authority::RolesManage) {
        return Err((StatusCode::FORBIDDEN, auth::AuthError::Forbidden.to_string()));
    }
    Ok(Json(state.roles.get(claims.workspace_id, id).await?))
}

/// A role may bundle only what its editor already holds.
///
/// Otherwise `roles:manage` is a ladder: define a role with everything, grant
/// it to yourself, climb. The check is against the editor's resolved
/// authorities, which for a system administrator is everything a workspace may
/// hold anyway.
fn within_reach(
    granted: &std::collections::HashSet<Authority>,
    wanted: &[String],
) -> Result<(), ApiError> {
    for name in wanted {
        let Some(a) = Authority::parse(name.trim()) else {
            // The store reports the bad name properly; this check only cares
            // about reach.
            continue;
        };
        if !granted.contains(&a) {
            return Err((
                StatusCode::FORBIDDEN,
                format!("you cannot put {a} in a role because you do not hold it yourself"),
            ));
        }
    }
    Ok(())
}

async fn create_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateRole>,
) -> Result<(StatusCode, Json<WorkspaceRole>), ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesManage).await?;
    // Reserved-to-the-platform first, so the answer names the real reason:
    // nobody holds `workspaces:create` in a workspace, and "you do not hold it"
    // would send the editor looking for someone who does.
    super::role::validate_authorities(&input.authorities)?;
    within_reach(&authorities_of(&state, &claims).await?, &input.authorities)?;
    let role = state.roles.create(claims.workspace_id, input).await?;
    tracing::info!(actor = %claims.subject, workspace_id = %claims.workspace_id, role = %role.name, "role created");
    Ok((StatusCode::CREATED, Json(role)))
}

async fn update_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRole>,
) -> Result<Json<WorkspaceRole>, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesManage).await?;
    if let Some(authorities) = &input.authorities {
        super::role::validate_authorities(authorities)?;
        within_reach(&authorities_of(&state, &claims).await?, authorities)?;
    }
    let role = state.roles.update(claims.workspace_id, id, input).await?;
    tracing::info!(actor = %claims.subject, workspace_id = %claims.workspace_id, role = %role.name, "role updated");
    Ok(Json(role))
}

async fn delete_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesManage).await?;
    state.roles.delete(claims.workspace_id, id).await?;
    tracing::info!(actor = %claims.subject, workspace_id = %claims.workspace_id, role_id = %id, "role deleted");
    Ok(StatusCode::NO_CONTENT)
}


// -- Usage --------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct UsageQuery {
    /// Inclusive start, RFC 3339.
    from: Option<chrono::DateTime<chrono::Utc>>,
    /// Exclusive end, RFC 3339. A closed month is `from` the first and `to`
    /// the first of the next.
    to: Option<chrono::DateTime<chrono::Utc>>,
    /// The `next` of the previous page.
    after: Option<Uuid>,
    limit: Option<i64>,
    /// System administrators may name a workspace; everyone else gets their own.
    workspace_id: Option<Uuid>,
}

/// The ledger, paged. What a bill is built from.
async fn export_usage(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageQuery>,
) -> Result<Json<super::usage::UsagePage>, ApiError> {
    let claims = authorize(&state, &headers, Authority::UsageRead).await?;
    let workspace_id = match query.workspace_id {
        Some(other) if other != claims.workspace_id => {
            if !claims.is_system_admin() {
                return Err((StatusCode::FORBIDDEN, "not your workspace's ledger".into()));
            }
            other
        }
        _ => claims.workspace_id,
    };
    let page = state
        .usage
        .export(
            workspace_id,
            query.from,
            query.to,
            query.after,
            query.limit.unwrap_or(500).clamp(1, 5000),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(page))
}


// -- Settings -----------------------------------------------------------------

use super::settings::{Level, Owner, SettingsError};

impl From<SettingsError> for ApiError {
    fn from(e: SettingsError) -> Self {
        let status = match e {
            SettingsError::Unknown(_) => StatusCode::NOT_FOUND,
            SettingsError::Invalid(_) => StatusCode::BAD_REQUEST,
            SettingsError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

#[derive(Debug, serde::Deserialize)]
struct SetSetting {
    value: serde_json::Value,
}

/// Whether the caller may write at `level`, and whether the setting allows it.
async fn may_write(state: &ApiState, claims: &SessionClaims, level: Level, key: &str) -> Result<(), ApiError> {
    let setting = super::settings::find(key).ok_or(SettingsError::Unknown(key.to_string()))?;
    match level {
        Level::Operator => {
            if !claims.is_system_admin() {
                return Err((StatusCode::FORBIDDEN, "only the operator sets platform defaults".into()));
            }
        }
        Level::Workspace(_) | Level::Agent { .. } => {
            require(state, claims, Authority::SettingsUpdate).await?;
            if setting.owner == Owner::OperatorOnly {
                return Err((
                    StatusCode::FORBIDDEN,
                    format!("{} is set by the operator and cannot be overridden here", setting.label),
                ));
            }
        }
    }
    Ok(())
}

async fn view_operator_settings(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<super::settings::Effective>>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    if !claims.is_system_admin() {
        return Err((StatusCode::FORBIDDEN, "only the operator sees platform defaults".into()));
    }
    Ok(Json(state.settings.view(Level::Operator).await?))
}

async fn set_operator_setting(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
    Json(input): Json<SetSetting>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_write(&state, &claims, Level::Operator, &key).await?;
    state.settings.set(Level::Operator, &key, input.value).await?;
    tracing::info!(actor = %claims.subject, key = %key, "platform setting set");
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_operator_setting(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_write(&state, &claims, Level::Operator, &key).await?;
    state.settings.clear(Level::Operator, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn view_workspace_settings(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<super::settings::Effective>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsRead).await?;
    Ok(Json(state.settings.view(Level::Workspace(claims.workspace_id)).await?))
}

async fn set_workspace_setting(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
    Json(input): Json<SetSetting>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let level = Level::Workspace(claims.workspace_id);
    may_write(&state, &claims, level, &key).await?;
    state.settings.set(level, &key, input.value).await?;
    tracing::info!(actor = %claims.subject, workspace_id = %claims.workspace_id, key = %key, "workspace setting set");
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_workspace_setting(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let level = Level::Workspace(claims.workspace_id);
    may_write(&state, &claims, level, &key).await?;
    state.settings.clear(level, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The agent must be the caller's workspace's, or the level would let a workspace
/// write settings onto somebody else's agent.
async fn agent_level(state: &ApiState, claims: &SessionClaims, agent_id: Uuid) -> Result<Level, ApiError> {
    state.agents.get(claims.workspace_id, agent_id).await?;
    Ok(Level::Agent { workspace_id: claims.workspace_id, agent_id })
}

async fn view_agent_settings(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(agent_id): Path<Uuid>,
) -> Result<Json<Vec<super::settings::Effective>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let level = agent_level(&state, &claims, agent_id).await?;
    Ok(Json(state.settings.view(level).await?))
}

async fn set_agent_setting(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((agent_id, key)): Path<(Uuid, String)>,
    Json(input): Json<SetSetting>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let level = agent_level(&state, &claims, agent_id).await?;
    may_write(&state, &claims, level, &key).await?;
    state.settings.set(level, &key, input.value).await?;
    tracing::info!(actor = %claims.subject, agent_id = %agent_id, key = %key, "agent setting set");
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_agent_setting(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((agent_id, key)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let level = agent_level(&state, &claims, agent_id).await?;
    may_write(&state, &claims, level, &key).await?;
    state.settings.clear(level, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/v1/login", post(super::login::login))
        .route("/v1/session/workspace", post(super::login::select_workspace))
        .route("/v1/logout", post(super::login::logout))
        .route("/v1/session/refresh", post(super::login::refresh))
        .route(
            "/v1/agents",
            get(super::agents::list_agents).post(super::agents::create_agent),
        )
        .route(
            "/v1/agent-sessions",
            get(super::sessions::list_sessions).post(super::sessions::create_session),
        )
        .route(
            "/v1/agent-sessions/{id}",
            axum::routing::delete(super::sessions::delete_session)
                .patch(super::sessions::rename_session),
        )
        .route(
            "/v1/agent-sessions/{id}/messages",
            get(super::sessions::get_messages).post(super::sessions::send_message),
        )
        // Stopping a turn, rather than a resource of its own: what is being
        // acted on is the conversation, and which job is answering it is the
        // platform's business rather than the caller's.
        .route(
            "/v1/agent-sessions/{id}/cancel",
            post(super::sessions::cancel_turn),
        )
        .route(
            "/v1/agents/{id}",
            get(super::agents::get_agent)
                .patch(super::agents::update_agent)
                .delete(super::agents::delete_agent),
        )
        .route("/v1/session", get(session_info))
        .route("/v1/events", get(super::events::poll))
        .route(
            "/v1/egress-rules",
            get(list_egress_rules).post(create_egress_rule),
        )
        .route(
            "/v1/egress-rules/{id}",
            axum::routing::delete(delete_egress_rule),
        )
        // Runtimes ask here for work and report back what it produced. Both
        // require GatewayInvoke, which is the platform's own tier rather than
        // a workspace's.
        .route("/v1/work", post(super::work::take))
        .route("/v1/work/{job_id}/events", post(super::work::report))
        .route("/v1/work/{job_id}/abandon", post(super::work::abandon))
        .route("/v1/agent-sessions/{id}/files", get(super::files::list))
        .route(
            "/v1/agent-sessions/{id}/files/{scope}/{*path}",
            get(super::files::download)
                .put(super::files::upload)
                .delete(super::files::delete)
                .layer(axum::extract::DefaultBodyLimit::max(super::files::MAX_UPLOAD_BYTES)),
        )
        .route("/v1/usage", get(export_usage))
        .route("/v1/settings", get(view_workspace_settings))
        .route(
            "/v1/settings/{key}",
            axum::routing::put(set_workspace_setting).delete(clear_workspace_setting),
        )
        .route("/v1/platform/skills", post(super::skills::create_platform_skill))
        .route(
            "/v1/platform/skills/{id}",
            axum::routing::patch(super::skills::update_platform_skill),
        )
        .route(
            "/v1/platform/skills/{id}/versions",
            post(super::skills::add_platform_version),
        )
        .route(
            "/v1/platform/skills/{id}/retired",
            axum::routing::put(super::skills::retire_platform_skill),
        )
        .route("/v1/platform/settings", get(view_operator_settings))
        .route(
            "/v1/platform/settings/{key}",
            axum::routing::put(set_operator_setting).delete(clear_operator_setting),
        )
        .route(
            "/v1/skills",
            get(super::skills::list_skills).post(super::skills::create_skill),
        )
        .route(
            "/v1/skills/{id}",
            get(super::skills::get_skill)
                .patch(super::skills::update_skill)
                .delete(super::skills::delete_skill),
        )
        .route("/v1/skills/{id}/retired", axum::routing::put(super::skills::retire_skill))
        .route(
            "/v1/skills/{id}/versions",
            get(super::skills::list_versions).post(super::skills::add_version),
        )
        .route(
            "/v1/skills/{id}/versions/{version_id}",
            get(super::skills::get_version),
        )
        .route("/v1/skills/{id}/fork", post(super::skills::fork_skill))
        .route(
            "/v1/skills/{id}/hosts/approve",
            post(super::skills::approve_skill_hosts),
        )
        .route(
            "/v1/agents/{id}/skills",
            get(super::skills::list_agent_skills).put(super::skills::set_agent_skills),
        )
        .route("/v1/agents/{id}/settings", get(view_agent_settings))
        .route(
            "/v1/agents/{id}/settings/{key}",
            axum::routing::put(set_agent_setting).delete(clear_agent_setting),
        )
        .route("/v1/authorities", get(list_authorities))
        .route("/v1/roles", get(list_roles).post(create_role))
        .route(
            "/v1/roles/{id}",
            get(get_role).patch(update_role).delete(delete_role),
        )
        .route("/v1/users", get(list_users).post(create_user))
        .route("/v1/users/{id}", get(get_user).patch(update_user).delete(delete_user))
        .route("/v1/users/{user_id}/identities", post(add_identity))
        .route(
            "/v1/users/{user_id}/identities/{identity_id}",
            axum::routing::delete(remove_identity),
        )
        .route(
            "/v1/users/{user_id}/workspaces/{workspace_id}/roles",
            post(grant_workspace_role),
        )
        .route(
            "/v1/users/{user_id}/workspaces/{workspace_id}/roles/{role}",
            axum::routing::delete(revoke_workspace_role),
        )
        .route("/v1/workspaces", get(list_workspaces).post(create_workspace))
        .route("/v1/workspaces/{id}", get(get_workspace).patch(update_workspace).delete(delete_workspace))
        .with_state(state)
}
