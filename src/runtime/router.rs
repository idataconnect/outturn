use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{self, Authority, TokenMinter, TokenValidator};

use super::sandbox::{Sandbox, SandboxConfig, SecurityTier};
use super::storage::StorageBackend;

pub struct RuntimeState {
    pub storage: Arc<dyn StorageBackend>,
    pub auth: TokenValidator,
    /// Mints the short-lived token the sandbox hands to the gateway on the
    /// guest's behalf. Separate from the caller's token so a guest's reach is
    /// bounded by what the runtime grants, not by what the API holds.
    pub minter: TokenMinter,
    pub gateway_url: String,
    /// WASM executed when an agent has no module of its own.
    pub default_module: Option<Vec<u8>>,
}

#[derive(Debug, Deserialize)]
pub struct ExecuteRequest {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    /// The chat request the guest should issue, as OpenAI-compatible JSON.
    pub request: serde_json::Value,
    #[serde(default)]
    pub security_tier: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecuteResponse {
    /// The gateway's reply, passed through unchanged.
    pub response: serde_json::Value,
}

type ApiError = (StatusCode, String);

pub async fn execute(
    State(state): State<Arc<RuntimeState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ExecuteRequest>,
) -> Result<Json<ExecuteResponse>, ApiError> {
    // The caller must be authorised to run agents in this tenant.
    let token = auth::extract_bearer(&headers)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    let claims = state
        .auth
        .validate(token)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;

    claims
        .require(Authority::GatewayInvoke)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    // The tenant on the token wins over the one in the body, so a caller
    // cannot run work against a tenant it does not hold a token for.
    if claims.tenant_id != request.tenant_id {
        return Err((
            StatusCode::FORBIDDEN,
            "tenant does not match the token".into(),
        ));
    }

    let Some(module) = state.default_module.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no agent module configured".into(),
        ));
    };

    // A fresh token per execution, carrying only what the guest needs.
    let gateway_token = state
        .minter
        .mint(request.session_id, request.tenant_id, &claims.roles)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let config = SandboxConfig {
        session_id: request.session_id,
        tenant_id: request.tenant_id,
        security_tier: match request.security_tier.as_deref() {
            Some("trusted") => SecurityTier::Trusted,
            _ => SecurityTier::Standard,
        },
        gateway_token,
        gateway_url: state.gateway_url.clone(),
        memory_limit_bytes: 128 * 1024 * 1024,
        fuel_limit: 10_000_000_000,
    };

    let sandbox = Sandbox::new(config, Arc::clone(&state.storage))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sandbox
        .run(module)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("guest failed: {e}")))?;

    Err((
        StatusCode::NOT_IMPLEMENTED,
        "guest ran, but returning its output is not wired yet".into(),
    ))
}

pub fn routes(state: Arc<RuntimeState>) -> Router {
    Router::new()
        .route("/v1/execute", post(execute))
        .with_state(state)
}
