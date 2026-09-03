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
    pub(super) pool: sqlx::PgPool,
    pub(super) bus: crate::events::EventBus,
    /// Fires on shutdown so parked long polls return instead of holding the
    /// drain open for their full timeout.
    pub(super) shutdown: Arc<tokio::sync::Notify>,
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
            pool,
            bus,
            shutdown,
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
    let token = auth::session_from_cookies(headers)
        .or_else(|| auth::extract_bearer(headers).ok())
        .ok_or((StatusCode::UNAUTHORIZED, "not authenticated".to_string()))?;

    state
        .auth
        .validate(token)
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
    let user = state.users.get(claims.session_id).await?;
    let tenants = state.users.memberships(claims.session_id).await?;

    Ok(Json(SessionInfo {
        session_id: claims.session_id,
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
        actor = %claims.session_id,
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
    tracing::info!(actor = %claims.session_id, tenant_id = %id, "tenant deleted");
    Ok(StatusCode::NO_CONTENT)
}


#[derive(Debug, serde::Deserialize)]
struct GrantRole {
    role: Role,
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
    tracing::info!(actor = %claims.session_id, user_id = %user.id, "user created");
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
    if claims.session_id == id {
        return Err((StatusCode::BAD_REQUEST, "cannot delete yourself".into()));
    }
    state.users.delete(id).await?;
    tracing::info!(actor = %claims.session_id, user_id = %id, "user deleted");
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
    if claims.session_id != user_id {
        claims
            .require(Authority::UsersUpdate)
            .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
    }

    let identity = state
        .users
        .add_password_identity(user_id, &input.email, &input.password)
        .await?;
    tracing::info!(actor = %claims.session_id, user_id = %user_id, "identity added");
    Ok((StatusCode::CREATED, Json(identity)))
}

async fn remove_identity(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((user_id, identity_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    if claims.session_id != user_id {
        claims
            .require(Authority::UsersUpdate)
            .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
    }

    state.users.remove_identity(user_id, identity_id).await?;
    tracing::info!(actor = %claims.session_id, user_id = %user_id, "identity removed");
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
        actor = %claims.session_id,
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
        actor = %claims.session_id,
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
