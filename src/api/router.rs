use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::{self, Authority, Role, SessionClaims, TokenMinter, TokenValidator};

use super::tenant::{CreateTenant, Tenant, TenantError, TenantStore};
use super::agent::AgentStore;
use super::chat::ChatStore;
use super::session::SessionStore;
use super::user::{CreateUser, Identity, TenantMembership, User, UserStore};

pub struct ApiState {
    pub(super) tenants: Arc<dyn TenantStore>,
    pub(super) users: Arc<dyn UserStore>,
    pub(super) sessions: Arc<dyn SessionStore>,
    pub(super) agents: Arc<dyn AgentStore>,
    pub(super) chat: Arc<dyn ChatStore>,
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

pub(super) fn authorize(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
    authority: Authority,
) -> Result<SessionClaims, ApiError> {
    let claims = authenticate(state, headers)?;
    claims
        .require(authority)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
    Ok(claims)
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
    let mut authorities: Vec<String> = auth::resolve_authorities(&claims.roles)
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
        roles: claims.roles.iter().map(|r| r.to_string()).collect(),
        authorities,
        tenants,
    }))
}

async fn list_tenants(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Tenant>>, ApiError> {
    authorize(&state, &headers, Authority::TenantsRead)?;
    Ok(Json(state.tenants.list().await?))
}

async fn create_tenant(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateTenant>,
) -> Result<(StatusCode, Json<Tenant>), ApiError> {
    let claims = authorize(&state, &headers, Authority::TenantsCreate)?;
    let tenant = state.tenants.create(input).await?;
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
    authorize(&state, &headers, Authority::TenantsRead)?;
    Ok(Json(state.tenants.get(id).await?))
}

async fn delete_tenant(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::TenantsDelete)?;
    state.tenants.delete(id).await?;
    tracing::info!(actor = %claims.subject, tenant_id = %id, "tenant deleted");
    Ok(StatusCode::NO_CONTENT)
}


#[derive(Debug, serde::Deserialize)]
struct GrantRole {
    role: Role,
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
    let claims = authorize(&state, &headers, Authority::SettingsRead)?;
    Ok(Json(super::egress::list(&state.pool, claims.tenant_id).await?))
}

async fn create_egress_rule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<super::egress::CreateRule>,
) -> Result<(StatusCode, Json<super::egress::Rule>), ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsUpdate)?;
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
    let claims = authorize(&state, &headers, Authority::SettingsUpdate)?;
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
    authorize(&state, &headers, Authority::UsersRead)?;
    Ok(Json(state.users.list().await?))
}

async fn create_user(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateUser>,
) -> Result<(StatusCode, Json<User>), ApiError> {
    let claims = authorize(&state, &headers, Authority::UsersCreate)?;
    let user = state.users.create(input).await?;
    tracing::info!(actor = %claims.subject, user_id = %user.id, "user created");
    Ok((StatusCode::CREATED, Json(user)))
}

async fn get_user(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<User>, ApiError> {
    authorize(&state, &headers, Authority::UsersRead)?;
    Ok(Json(state.users.get(id).await?))
}

async fn delete_user(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::UsersDelete)?;
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
        claims
            .require(Authority::UsersUpdate)
            .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
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
        claims
            .require(Authority::UsersUpdate)
            .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
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
    let claims = authorize(&state, &headers, Authority::RolesAssign)?;
    // A tenant admin may only grant within their own tenant; a system admin
    // carries the authority in whichever tenant they are scoped to.
    if claims.tenant_id != tenant_id && !claims.roles.contains(&Role::SystemAdmin) {
        return Err((StatusCode::FORBIDDEN, "cannot grant outside your tenant".into()));
    }
    if input.role == Role::SystemAdmin {
        return Err((
            StatusCode::BAD_REQUEST,
            "system_admin is not a tenant role".into(),
        ));
    }
    state
        .users
        .grant_tenant_role(user_id, tenant_id, input.role)
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
    let claims = authorize(&state, &headers, Authority::RolesAssign)?;
    if claims.tenant_id != tenant_id && !claims.roles.contains(&Role::SystemAdmin) {
        return Err((StatusCode::FORBIDDEN, "cannot revoke outside your tenant".into()));
    }
    let role: Role = role
        .parse()
        .map_err(|e: String| (StatusCode::BAD_REQUEST, e))?;
    state
        .users
        .revoke_tenant_role(user_id, tenant_id, role)
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
        .route("/v1/users", get(list_users).post(create_user))
        .route("/v1/users/{id}", get(get_user).delete(delete_user))
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
        .route("/v1/tenants/{id}", get(get_tenant).delete(delete_tenant))
        .with_state(state)
}
