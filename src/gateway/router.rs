use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use futures::StreamExt;

use crate::auth::{self, TokenValidator, SessionClaims};

use super::breaker;
use super::llm::provider::{LlmProvider, ProviderError};
use super::llm::types::{ChatCompletionRequest, ChatCompletionResponse};

pub struct GatewayState {
    providers: Vec<Arc<dyn LlmProvider>>,
    auth: TokenValidator,
    /// Backs the shared circuit breaker. Optional so the gateway still runs
    /// without a database -- it simply calls every provider, which is the
    /// behaviour it had before the breaker existed.
    health: Option<sqlx::postgres::PgPool>,
}

impl GatewayState {
    pub fn new(providers: Vec<Arc<dyn LlmProvider>>, auth: TokenValidator) -> Self {
        Self {
            providers,
            auth,
            health: None,
        }
    }

    pub fn with_health(mut self, pool: sqlx::postgres::PgPool) -> Self {
        self.health = Some(pool);
        self
    }

    /// Whether this provider may be called, and who owns the next probe.
    async fn admits(&self, provider: &Arc<dyn LlmProvider>) -> bool {
        let Some(pool) = &self.health else {
            return true;
        };
        breaker::check(pool, &provider.endpoint()).await == breaker::Verdict::Allow
    }

    /// Feeds the outcome of a call back into the shared breaker.
    async fn observe(&self, provider: &Arc<dyn LlmProvider>, outcome: Result<(), &ProviderError>) {
        let Some(pool) = &self.health else {
            return;
        };
        let endpoint = provider.endpoint();
        match outcome {
            Ok(()) => breaker::record_success(pool, &endpoint).await,
            Err(e) if breaker::counts_as_failure(e) => {
                breaker::record_failure(pool, &endpoint, &e.to_string()).await
            }
            // A rejected request or a rate limit says the provider is alive.
            Err(_) => {}
        }
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
        if !provider.is_available().await || !state.admits(provider).await {
            continue;
        }

        match provider.chat_completion(&request).await {
            Ok(response) => {
                state.observe(provider, Ok(())).await;
                return Ok(Json(response));
            }
            Err(e) => {
                tracing::warn!(provider = ?provider.provider(), error = %e, "provider failed");
                state.observe(provider, Err(&e)).await;
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

/// Streams a completion as newline-delimited JSON.
///
/// NDJSON rather than Server-Sent Events: the caller is the runtime, not a
/// browser, and SSE's framing buys nothing here. The provider's SSE is
/// unwrapped one layer down, so it terminates at the gateway.
async fn chat_completions_stream(
    State(state): State<Arc<GatewayState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    let claims = authenticate(&state, &headers)?;

    tracing::debug!(
        session_id = %claims.session_id,
        tenant_id = %claims.tenant_id,
        model = %request.model,
        "streaming chat completion request"
    );

    for provider in &state.providers {
        if !provider.is_available().await || !state.admits(provider).await {
            continue;
        }

        match provider.chat_completion_stream(&request).await {
            Ok(chunks) => {
                // Recorded once the provider has accepted and begun
                // streaming. A stream that dies partway is not seen here --
                // the body is handed to the caller and this scope ends -- so
                // the breaker measures reachability, not completion.
                state.observe(provider, Ok(())).await;
                let body = chunks.map(|chunk| match chunk {
                    Ok(chunk) => serde_json::to_string(&chunk)
                        .map(|mut line| {
                            line.push('\n');
                            axum::body::Bytes::from(line)
                        })
                        .map_err(|e| std::io::Error::other(e.to_string())),
                    Err(e) => Err(std::io::Error::other(e.to_string())),
                });

                return Ok((
                    [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
                    axum::body::Body::from_stream(body),
                )
                    .into_response());
            }
            Err(e) => {
                tracing::warn!(provider = ?provider.provider(), error = %e, "stream failed");
                state.observe(provider, Err(&e)).await;
                continue;
            }
        }
    }

    Err((StatusCode::BAD_GATEWAY, "no provider could stream".into()))
}

pub fn routes(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/chat/completions/stream", post(chat_completions_stream))
        .with_state(state)
}
