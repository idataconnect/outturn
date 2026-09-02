use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

use crate::auth::{self, TokenValidator, SessionClaims};

use super::llm::provider::{LlmProvider, ProviderError};
use super::llm::types::{ChatCompletionRequest, ChatCompletionResponse};

pub struct GatewayState {
    providers: Vec<Arc<dyn LlmProvider>>,
    auth: TokenValidator,
}

impl GatewayState {
    pub fn new(providers: Vec<Arc<dyn LlmProvider>>, auth: TokenValidator) -> Self {
        Self { providers, auth }
    }
}

fn authenticate(
    state: &GatewayState,
    headers: &axum::http::HeaderMap,
) -> Result<SessionClaims, (StatusCode, String)> {
    let token = auth::extract_bearer(headers)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    let claims = state
        .auth
        .validate(token)
        .map_err(|e| match e {
            auth::AuthError::Forbidden => (StatusCode::FORBIDDEN, e.to_string()),
            _ => (StatusCode::UNAUTHORIZED, e.to_string()),
        })?;
    claims
        .require(auth::Authority::GatewayInvoke)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
    Ok(claims)
}

async fn chat_completions(
    State(state): State<Arc<GatewayState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Json<ChatCompletionResponse>, (StatusCode, String)> {
    let claims = authenticate(&state, &headers)?;

    tracing::debug!(
        session_id = %claims.session_id,
        tenant_id = %claims.tenant_id,
        model = %request.model,
        "chat completion request"
    );

    let mut last_error = None;

    for provider in &state.providers {
        if !provider.is_available().await {
            continue;
        }

        match provider.chat_completion(&request).await {
            Ok(response) => return Ok(Json(response)),
            Err(ProviderError::RateLimited) | Err(ProviderError::Unavailable) => {
                tracing::warn!(
                    provider = ?provider.provider(),
                    "provider unavailable, trying next"
                );
                last_error = Some(ProviderError::Unavailable);
                continue;
            }
            Err(e) => {
                tracing::error!(provider = ?provider.provider(), error = %e, "provider error");
                last_error = Some(e);
                continue;
            }
        }
    }

    let msg = match last_error {
        Some(e) => format!("all providers failed, last error: {e}"),
        None => "no providers configured".into(),
    };
    Err((StatusCode::BAD_GATEWAY, msg))
}

pub fn routes(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state)
}
