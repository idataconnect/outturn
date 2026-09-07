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

use super::tenant::{CreateTenant, Tenant, TenantError, TenantStore};
use super::agent::AgentStore;
use super::chat::ChatStore;
use super::session::SessionStore;
use super::role::{CreateRole, RoleError, RoleStore, TenantRole, UpdateRole};
use super::user::{CreateUser, Identity, TenantMembership, User, UserStore};

pub struct ApiState {
    pub(super) tenants: Arc<dyn TenantStore>,
    pub(super) users: Arc<dyn UserStore>,
    pub(super) sessions: Arc<dyn SessionStore>,
    pub(super) agents: Arc<dyn AgentStore>,
    pub(super) chat: Arc<dyn ChatStore>,
    /// What a tenant's roles mean. Consulted on every authorised request.
    pub(super) roles: Arc<dyn RoleStore>,
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
        tenants: Arc<dyn TenantStore>,
        users: Arc<dyn UserStore>,
        sessions: Arc<dyn SessionStore>,
        agents: Arc<dyn AgentStore>,
        chat: Arc<dyn ChatStore>,
        roles: Arc<dyn RoleStore>,
        auth: TokenValidator,
        minter: TokenMinter,
        runtime_key: crate::auth::RuntimeKey,
        pool: sqlx::PgPool,
        bus: crate::events::EventBus,
        shutdown: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            tenants,
            users,
            sessions,
            agents,
            chat,
            roles,
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
/// Platform roles resolve in code; tenant roles resolve through the role
/// store, which caches per tenant. Done per request rather than at mint so an
/// edit to a role takes effect on the next click, not at the next refresh.
pub(super) async fn authorities_of(
    state: &ApiState,
    claims: &SessionClaims,
) -> Result<std::collections::HashSet<Authority>, ApiError> {
    let mut granted = auth::platform_authorities(&claims.roles);
    granted.extend(
        state
            .roles
            .authorities_for(claims.tenant_id, &claims.roles)
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

impl From<TenantError> for ApiError {
    fn from(e: TenantError) -> Self {
        let status = match e {
            TenantError::NotFound => StatusCode::NOT_FOUND,
            TenantError::DuplicateSlug(_) => StatusCode::CONFLICT,
            TenantError::Invalid(_) => StatusCode::BAD_REQUEST,
            TenantError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

/// Everything the UI needs to restore a session on a cold load.
///
/// Deliberately includes the display name and memberships, not just ids: a
/// reload has only this endpoint to work from, and a client left to stitch the
/// identity together from several calls renders a signed-in user as nameless
/// and tenantless until they all land -- which is precisely what it did.
#[derive(Debug, Serialize)]
struct SessionInfo {
    session_id: Uuid,
    tenant_id: Uuid,
    display_name: String,
    roles: Vec<String>,
    authorities: Vec<String>,
    tenants: Vec<TenantMembership>,
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
    let tenants = state.users.memberships(claims.subject).await?;

    Ok(Json(SessionInfo {
        session_id: claims.subject,
        tenant_id: claims.tenant_id,
        display_name: user.display_name,
        roles: claims.roles.clone(),
        authorities,
        tenants,
    }))
}

async fn list_tenants(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Tenant>>, ApiError> {
    authorize(&state, &headers, Authority::TenantsRead).await?;
    Ok(Json(state.tenants.list().await?))
}

async fn create_tenant(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateTenant>,
) -> Result<(StatusCode, Json<Tenant>), ApiError> {
    let claims = authorize(&state, &headers, Authority::TenantsCreate).await?;
    let tenant = state.tenants.create(input).await?;
    // A tenant with no roles is one nobody can be given access to.
    state.roles.seed_defaults(tenant.id).await?;
    tracing::info!(
        actor = %claims.subject,
        tenant_id = %tenant.id,
        slug = %tenant.slug,
        "tenant created"
    );
    Ok((StatusCode::CREATED, Json(tenant)))
}

async fn get_tenant(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Tenant>, ApiError> {
    authorize(&state, &headers, Authority::TenantsRead).await?;
    Ok(Json(state.tenants.get(id).await?))
}

async fn update_tenant(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<super::tenant::UpdateTenant>,
) -> Result<Json<Tenant>, ApiError> {
    let claims = authorize(&state, &headers, Authority::TenantsUpdate).await?;
    let tenant = state.tenants.rename(id, &input.name).await?;
    tracing::info!(actor = %claims.subject, tenant_id = %id, "tenant renamed");
    Ok(Json(tenant))
}

async fn delete_tenant(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::TenantsDelete).await?;
    state.tenants.delete(id).await?;
    tracing::info!(actor = %claims.subject, tenant_id = %id, "tenant deleted");
    Ok(StatusCode::NO_CONTENT)
}


#[derive(Debug, serde::Deserialize)]
struct GrantRole {
    /// The name of one of the tenant's roles.
    role: String,
}

/// The hosts this tenant's agents may reach.
///
/// Scoped to the caller's own tenant throughout, taken from the token rather
/// than from a path: an egress list is the shape of what a tenant's agents can
/// reach, and reading somebody else's tells you where to aim an injection.
async fn list_egress_rules(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<super::egress::Rule>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsRead).await?;
    Ok(Json(super::egress::list(&state.pool, claims.tenant_id).await?))
}

async fn create_egress_rule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<super::egress::CreateRule>,
) -> Result<(StatusCode, Json<super::egress::Rule>), ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsUpdate).await?;
    let rule = super::egress::create(&state.pool, claims.tenant_id, input).await?;
    // Worth a line in the log on its own: this is the moment a tenant's agents
    // gained somewhere new to send things.
    tracing::info!(
        actor = %claims.subject,
        tenant_id = %claims.tenant_id,
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
    if !super::egress::delete(&state.pool, claims.tenant_id, id).await? {
        return Err((StatusCode::NOT_FOUND, "no such rule".into()));
    }
    tracing::info!(
        actor = %claims.subject,
        tenant_id = %claims.tenant_id,
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
    // A tenant's administrator sees the accounts in their tenant. Every
    // account on the platform is a system administrator's view alone: the
    // tenant is the isolation boundary, and a user list that crossed it
    // named every other customer's staff.
    if claims.is_system_admin() {
        Ok(Json(state.users.list().await?))
    } else {
        Ok(Json(state.users.list_for_tenant(claims.tenant_id).await?))
    }
}

/// A user, with where they belong.
///
/// Memberships are narrowed to what the caller may know about: their own
/// tenant, or every tenant for a system administrator.
#[derive(Debug, serde::Serialize)]
struct UserDetail {
    #[serde(flatten)]
    user: User,
    memberships: Vec<TenantMembership>,
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

/// A new account, and optionally a role for it in the creator's tenant.
///
/// The role rides in the same request because the user list is scoped to the
/// tenant: an account created without a role there is one its creator can no
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
            .grant_tenant_role(user.id, claims.tenant_id, &role)
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
        memberships.retain(|m| m.tenant_id == claims.tenant_id);
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

async fn grant_tenant_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((user_id, tenant_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<GrantRole>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesAssign).await?;
    // A tenant admin may only grant within their own tenant; a system admin
    // carries the authority in whichever tenant they are scoped to.
    if claims.tenant_id != tenant_id && !claims.is_system_admin() {
        return Err((StatusCode::FORBIDDEN, "cannot grant outside your tenant".into()));
    }
    state
        .users
        .grant_tenant_role(user_id, tenant_id, &input.role)
        .await?;
    tracing::info!(
        actor = %claims.subject,
        user_id = %user_id,
        tenant_id = %tenant_id,
        role = %input.role,
        "tenant role granted"
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_tenant_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((user_id, tenant_id, role)): Path<(Uuid, Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesAssign).await?;
    if claims.tenant_id != tenant_id && !claims.is_system_admin() {
        return Err((StatusCode::FORBIDDEN, "cannot revoke outside your tenant".into()));
    }
    state
        .users
        .revoke_tenant_role(user_id, tenant_id, &role)
        .await?;
    tracing::info!(
        actor = %claims.subject,
        user_id = %user_id,
        tenant_id = %tenant_id,
        role = %role,
        "tenant role revoked"
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
            .filter(|a| a.tenant_assignable())
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
) -> Result<Json<Vec<TenantRole>>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let granted = authorities_of(&state, &claims).await?;
    if !granted.contains(&Authority::RolesAssign) && !granted.contains(&Authority::RolesManage) {
        return Err((StatusCode::FORBIDDEN, auth::AuthError::Forbidden.to_string()));
    }
    Ok(Json(state.roles.list(claims.tenant_id).await?))
}

async fn get_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<TenantRole>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let granted = authorities_of(&state, &claims).await?;
    if !granted.contains(&Authority::RolesAssign) && !granted.contains(&Authority::RolesManage) {
        return Err((StatusCode::FORBIDDEN, auth::AuthError::Forbidden.to_string()));
    }
    Ok(Json(state.roles.get(claims.tenant_id, id).await?))
}

/// A role may bundle only what its editor already holds.
///
/// Otherwise `roles:manage` is a ladder: define a role with everything, grant
/// it to yourself, climb. The check is against the editor's resolved
/// authorities, which for a system administrator is everything a tenant may
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
) -> Result<(StatusCode, Json<TenantRole>), ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesManage).await?;
    // Reserved-to-the-platform first, so the answer names the real reason:
    // nobody holds `tenants:create` in a tenant, and "you do not hold it"
    // would send the editor looking for someone who does.
    super::role::validate_authorities(&input.authorities)?;
    within_reach(&authorities_of(&state, &claims).await?, &input.authorities)?;
    let role = state.roles.create(claims.tenant_id, input).await?;
    tracing::info!(actor = %claims.subject, tenant_id = %claims.tenant_id, role = %role.name, "role created");
    Ok((StatusCode::CREATED, Json(role)))
}

async fn update_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRole>,
) -> Result<Json<TenantRole>, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesManage).await?;
    if let Some(authorities) = &input.authorities {
        super::role::validate_authorities(authorities)?;
        within_reach(&authorities_of(&state, &claims).await?, authorities)?;
    }
    let role = state.roles.update(claims.tenant_id, id, input).await?;
    tracing::info!(actor = %claims.subject, tenant_id = %claims.tenant_id, role = %role.name, "role updated");
    Ok(Json(role))
}

async fn delete_role(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::RolesManage).await?;
    state.roles.delete(claims.tenant_id, id).await?;
    tracing::info!(actor = %claims.subject, tenant_id = %claims.tenant_id, role_id = %id, "role deleted");
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/v1/login", post(super::login::login))
        .route("/v1/session/tenant", post(super::login::select_tenant))
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
            axum::routing::delete(super::sessions::delete_session),
        )
        .route(
            "/v1/agent-sessions/{id}/messages",
            get(super::sessions::get_messages).post(super::sessions::send_message),
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
        // a tenant's.
        .route("/v1/work", post(super::work::take))
        .route("/v1/work/{job_id}/events", post(super::work::report))
        .route("/v1/work/{job_id}/abandon", post(super::work::abandon))
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
            "/v1/users/{user_id}/tenants/{tenant_id}/roles",
            post(grant_tenant_role),
        )
        .route(
            "/v1/users/{user_id}/tenants/{tenant_id}/roles/{role}",
            axum::routing::delete(revoke_tenant_role),
        )
        .route("/v1/tenants", get(list_tenants).post(create_tenant))
        .route("/v1/tenants/{id}", get(get_tenant).patch(update_tenant).delete(delete_tenant))
        .with_state(state)
}
