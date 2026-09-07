use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{Json, extract::State, http::{StatusCode, header}};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth;

use super::router::{ApiError, ApiState};
use super::session::{IssuedRefresh, REFRESH_LIFETIME_SECS, SessionError};
use super::user::{TenantMembership, UserError};

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    /// Optional: when omitted the caller gets the tenant list and picks one.
    pub tenant_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginResponse {
    /// Credentials were good but no tenant was chosen; the UI shows a picker.
    SelectTenant {
        user_id: Uuid,
        display_name: String,
        tenants: Vec<TenantMembership>,
    },
    Authenticated {
        user_id: Uuid,
        display_name: String,
        tenant_id: Uuid,
        roles: Vec<String>,
        tenants: Vec<TenantMembership>,
    },
}

impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        let status = match e {
            // A replay is a real signal, but the caller is told only that they
            // must sign in again: naming it would tell an attacker they were
            // detected.
            SessionError::Invalid | SessionError::Replayed => StatusCode::UNAUTHORIZED,
            SessionError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

impl From<UserError> for ApiError {
    fn from(e: UserError) -> Self {
        let status = match e {
            UserError::NotFound | UserError::IdentityNotFound => StatusCode::NOT_FOUND,
            UserError::DuplicateEmail(_) => StatusCode::CONFLICT,
            UserError::Invalid(_) => StatusCode::BAD_REQUEST,
            UserError::BadCredentials => StatusCode::UNAUTHORIZED,
            UserError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

pub async fn login(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let user = state
        .users
        .authenticate(&request.email, &request.password)
        .await?;

    let tenants = state.users.memberships(user.id).await?;

    let Some(tenant_id) = request.tenant_id else {
        return Ok(Json(LoginResponse::SelectTenant {
            user_id: user.id,
            display_name: user.display_name,
            tenants,
        })
        .into_response());
    };

    // Selecting a tenant the user has no standing in must not mint a token.
    if !tenants.iter().any(|t| t.tenant_id == tenant_id) {
        return Err((StatusCode::FORBIDDEN, "no access to that tenant".into()));
    }

    let roles = state.users.roles_for_tenant(user.id, tenant_id).await?;
    if roles.is_empty() {
        return Err((StatusCode::FORBIDDEN, "no roles in that tenant".into()));
    }

    let token = mint(state.as_ref(), user.id, tenant_id, &roles)?;
    let refresh = state
        .sessions
        .issue(user.id, tenant_id, user_agent(&headers))
        .await?;

    Ok(authenticated_response(
        &token,
        Some(&refresh),
        LoginResponse::Authenticated {
            user_id: user.id,
            display_name: user.display_name,
            tenant_id,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            tenants,
        },
    ))
}

/// Returns the session in HttpOnly cookies rather than the body, so page
/// JavaScript never holds either token.
fn authenticated_response(
    token: &str,
    refresh: Option<&IssuedRefresh>,
    body: LoginResponse,
) -> Response {
    // A HeaderMap rather than a list: two Set-Cookie headers share a name, and
    // only append preserves both.
    let mut headers = axum::http::HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        auth::session_cookie(token, auth::SESSION_TOKEN_LIFETIME_SECS),
    );
    if let Some(refresh) = refresh {
        headers.append(
            header::SET_COOKIE,
            auth::refresh_cookie(&refresh.token, REFRESH_LIFETIME_SECS),
        );
    }
    (headers, Json(body)).into_response()
}

fn user_agent(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers.get(header::USER_AGENT)?.to_str().ok()
}

/// Exchanges the refresh cookie for a fresh access token, rotating the refresh
/// token in the process.
///
/// Roles are re-read here rather than carried over, so a revoked grant takes
/// effect within one access-token lifetime instead of lasting the whole
/// session.
pub async fn refresh(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    let presented = auth::refresh_from_cookies(&headers)
        .ok_or((StatusCode::UNAUTHORIZED, "no refresh token".to_string()))?;

    let rotated = state
        .sessions
        .rotate(presented, user_agent(&headers))
        .await?;

    let user = state.users.get(rotated.session.user_id).await?;
    let roles = state
        .users
        .roles_for_tenant(rotated.session.user_id, rotated.session.tenant_id)
        .await?;

    if roles.is_empty() {
        // Access was removed while the session was live.
        state.sessions.revoke(&rotated.token).await?;
        return Err((StatusCode::FORBIDDEN, "no roles in that tenant".into()));
    }

    let token = mint(
        state.as_ref(),
        rotated.session.user_id,
        rotated.session.tenant_id,
        &roles,
    )?;
    let tenants = state.users.memberships(rotated.session.user_id).await?;

    Ok(authenticated_response(
        &token,
        Some(&rotated),
        LoginResponse::Authenticated {
            user_id: rotated.session.user_id,
            display_name: user.display_name,
            tenant_id: rotated.session.tenant_id,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            tenants,
        },
    ))
}

/// Clears both cookies and ends the session server-side.
pub async fn logout(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    // Clearing the cookie alone would leave the refresh token usable.
    if let Some(token) = auth::refresh_from_cookies(&headers) {
        state.sessions.revoke(token).await?;
    }

    let mut headers = axum::http::HeaderMap::new();
    headers.append(header::SET_COOKIE, auth::clear_session_cookie());
    headers.append(header::SET_COOKIE, auth::clear_refresh_cookie());

    Ok((StatusCode::NO_CONTENT, headers).into_response())
}

#[derive(Debug, Deserialize)]
pub struct SelectTenantRequest {
    pub tenant_id: Uuid,
}

/// Re-mints the caller's token against a different tenant. Used by the tenant
/// dropdown, so switching does not require re-entering a password.
pub async fn select_tenant(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<SelectTenantRequest>,
) -> Result<Response, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let user = state.users.get(claims.subject).await?;

    let tenants = state.users.memberships(user.id).await?;
    if !tenants.iter().any(|t| t.tenant_id == request.tenant_id) {
        return Err((StatusCode::FORBIDDEN, "no access to that tenant".into()));
    }

    let roles = state
        .users
        .roles_for_tenant(user.id, request.tenant_id)
        .await?;
    if roles.is_empty() {
        return Err((StatusCode::FORBIDDEN, "no roles in that tenant".into()));
    }

    let token = mint(state.as_ref(), user.id, request.tenant_id, &roles)?;

    // The refresh token carries the tenant, so switching starts a new family
    // and retires the old session.
    if let Some(old) = auth::refresh_from_cookies(&headers) {
        state.sessions.revoke(old).await?;
    }
    let refresh = state
        .sessions
        .issue(user.id, request.tenant_id, user_agent(&headers))
        .await?;

    Ok(authenticated_response(
        &token,
        Some(&refresh),
        LoginResponse::Authenticated {
            user_id: user.id,
            display_name: user.display_name,
            tenant_id: request.tenant_id,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            tenants,
        },
    ))
}

fn mint(
    state: &ApiState,
    user_id: Uuid,
    tenant_id: Uuid,
    roles: &[String],
) -> Result<String, ApiError> {
    state
        .minter
        .mint_session(user_id, tenant_id, roles)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
