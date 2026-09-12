use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use futures::StreamExt;

use crate::auth::{self, TokenValidator, SessionClaims};

use super::breaker;
use super::routing::{self};
use super::llm::provider::{LlmProvider, ProviderError};
use super::llm::types::{ChatCompletionRequest, ChatCompletionResponse};

pub struct GatewayState {
    providers: Vec<Arc<dyn LlmProvider>>,
    auth: TokenValidator,
    /// Backs the shared circuit breaker and traffic routing. Optional so the
    /// gateway still runs without a database -- it falls back to the
    /// statically configured providers, which is what it did before either
    /// existed.
    health: Option<sqlx::postgres::PgPool>,
    providers_by_endpoint: routing::ProviderCache,
}

/// One thing to try: a provider, and the model to ask it for.
///
/// The model is carried alongside because a route decides it. When routing is
/// not configured the caller's own choice stands, which is how this behaved
/// before traffic types existed.
struct Attempt {
    provider: Arc<dyn LlmProvider>,
    model: Option<String>,
}

impl GatewayState {
    pub fn new(providers: Vec<Arc<dyn LlmProvider>>, auth: TokenValidator) -> Self {
        Self {
            providers,
            auth,
            health: None,
            providers_by_endpoint: routing::ProviderCache::default(),
        }
    }

    pub fn with_health(mut self, pool: sqlx::postgres::PgPool) -> Self {
        self.health = Some(pool);
        self
    }

    /// Whether this session's turn has been asked to stop.
    ///
    /// Answered here because the gateway is the tier holding a database
    /// connection: the runtime has none, and this is the one place already
    /// talking to both it and the provider. One indexed lookup per round,
    /// which is the same cadence steering is already taken at.
    ///
    /// A store that is not configured answers "no", which is what the gateway
    /// does about everything it cannot look up: the turn proceeds rather than
    /// being stopped by an outage in the thing that would have stopped it.
    async fn cancel_requested(&self, workspace_id: uuid::Uuid, session_id: uuid::Uuid) -> bool {
        let Some(pool) = &self.health else {
            return false;
        };
        match crate::jobs::live_turn_for_session(pool, workspace_id, session_id).await {
            Ok(Some(job_id)) => crate::jobs::cancel_requested(pool, job_id)
                .await
                .unwrap_or(false),
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(error = %e, "could not check whether a turn was cancelled");
                false
            }
        }
    }

    /// Anything the user has said since this turn began.
    ///
    /// Empty without a database or without a reply to attribute it to, which
    /// means steering simply does not happen rather than the call failing.
    async fn take_pending(
        &self,
        workspace_id: uuid::Uuid,
        session_id: uuid::Uuid,
        reply: Option<uuid::Uuid>,
    ) -> Vec<routing::Pending> {
        let (Some(pool), Some(reply)) = (&self.health, reply) else {
            return Vec::new();
        };
        match routing::take_pending(pool, session_id, reply).await {
            Ok(pending) => {
                // Told to the browser as well as to the guest. A message
                // taken mid-turn never gets a reply of its own, and without
                // this the reader watches it sit "queued" for ever.
                for message in &pending {
                    if let Err(e) = crate::events::append(
                        pool,
                        workspace_id,
                        Some(session_id),
                        "chat.absorbed",
                        serde_json::json!({
                            "message_id": message.id,
                            "absorbed_by": reply,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "could not announce an absorbed message");
                    }
                }
                pending
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not read pending input");
                Vec::new()
            }
        }
    }

    /// What to try, in order of precedence.
    ///
    /// A configured route list wins. With none -- no database, or nothing
    /// configured for this traffic type -- the statically built providers are
    /// used in their configured order, so an unconfigured system behaves as it
    /// always has rather than refusing to serve.
    async fn attempts(&self, workspace_id: uuid::Uuid, traffic_type: &str) -> Vec<Attempt> {
        if let Some(pool) = &self.health {
            match routing::routes_for(pool, workspace_id, traffic_type).await {
                Ok(routes) if !routes.is_empty() => {
                    let mut attempts = Vec::with_capacity(routes.len());
                    for route in routes {
                        if let Some(provider) = self.providers_by_endpoint.get(&route).await {
                            attempts.push(Attempt {
                                provider,
                                model: Some(route.model),
                            });
                        }
                    }
                    return attempts;
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "could not read routes, using static providers"),
            }
        }

        self.providers
            .iter()
            .map(|p| Attempt {
                provider: Arc::clone(p),
                model: None,
            })
            .collect()
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

/// The class of traffic this request belongs to.
///
/// A header rather than a body field: the request body is serialised straight
/// through to the provider, so anything added there leaks upstream. Routing is
/// also not something a model should be told about.
pub const TRAFFIC_HEADER: &str = "x-outturn-traffic";

/// The reply the caller is writing, so a message taken mid-turn can name what
/// absorbed it.
const REPLY_HEADER: &str = "x-outturn-reply";

fn reply_id(headers: &axum::http::HeaderMap) -> Option<uuid::Uuid> {
    headers
        .get(REPLY_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

fn traffic_type(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(TRAFFIC_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(routing::DEFAULT_TRAFFIC_TYPE)
        .to_string()
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
    // The gateway has no role store and needs none: the only role a token it
    // accepts can carry is the platform's `turn`.
    claims
        .require_platform(auth::Authority::GatewayInvoke)
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
        session_id = %claims.subject,
        workspace_id = %claims.workspace_id,
        model = %request.model,
        "chat completion request"
    );

    let mut last_error = None;
    let traffic = traffic_type(&headers);

    for attempt in state.attempts(claims.workspace_id, &traffic).await {
        let provider = &attempt.provider;
        if !state.admits(provider).await {
            continue;
        }

        // The route decides the model when there is one; otherwise the
        // caller's choice stands.
        let mut request = request.clone();
        if let Some(model) = &attempt.model {
            request.model = model.clone();
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

/// How often a running stream asks whether it has been told to stop.
///
/// A token arrives every few milliseconds and a query per token would cost
/// more than the generation does. This is the delay somebody sees between
/// pressing stop and the words ceasing, traded against a database round trip
/// on every chunk of every stream.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

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
        session_id = %claims.subject,
        workspace_id = %claims.workspace_id,
        model = %request.model,
        "streaming chat completion request"
    );

    let traffic = traffic_type(&headers);

    // Taken before the call rather than after, so a long generation does not
    // hold a steering message for its whole duration. Anything arriving during
    // this call is picked up by the next one, which is the next round -- the
    // only boundary where injecting it is safe anyway.
    let pending = state
        .take_pending(claims.workspace_id, claims.subject, reply_id(&headers))
        .await;

    for attempt in state.attempts(claims.workspace_id, &traffic).await {
        let provider = &attempt.provider;
        if !state.admits(provider).await {
            continue;
        }

        let mut request = request.clone();
        if let Some(model) = &attempt.model {
            request.model = model.clone();
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

                // Stops relaying the moment somebody asks, and -- because
                // ending the stream drops the provider's response body --
                // closes the connection it was arriving on. That is the only
                // stop a provider understands: there is no call to make that
                // means "never mind", so hanging up is the request.
                //
                // Best-effort by nature. A provider may finish generating and
                // bill for it regardless, and nothing here can find out.
                //
                // Checked on a tick rather than per chunk: a token arrives
                // every few milliseconds and a database round trip each time
                // would cost more than the generation. Between ticks the
                // stream keeps flowing, which is the latency this trades for.
                // Per request, not shared: two sessions streaming at once must
                // not be able to stop one another.
                let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let watcher = Arc::clone(&flag);
                let stopper = Arc::clone(&state);
                let workspace_id = claims.workspace_id;
                let session_id = claims.subject;

                // One task asking, on a tick, for as long as the stream runs.
                // It ends when the stream is dropped, because the flag it
                // writes to is the only thing keeping it alive.
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(CANCEL_POLL).await;
                        if Arc::strong_count(&watcher) == 1 {
                            // Nothing is reading it any more: the stream has
                            // ended, one way or another.
                            return;
                        }
                        if stopper.cancel_requested(workspace_id, session_id).await {
                            watcher.store(true, std::sync::atomic::Ordering::Relaxed);
                            return;
                        }
                    }
                });

                let cutter = Arc::clone(&flag);
                let body = body.take_while(move |_| {
                    std::future::ready(!cutter.load(std::sync::atomic::Ordering::Relaxed))
                });

                // Appended after the provider's chunks, in the same framing.
                // A caller that does not know this line exists ignores it, so
                // an older runtime keeps working -- it simply does not steer.
                //
                // The cancel rides here rather than in a channel of its own
                // for the same reason: cutting the stream stops the words, but
                // only this tells the turn *why* they stopped, which is the
                // difference between ending deliberately and looking like a
                // provider that hung up.
                let announce = Arc::clone(&flag);
                let trailer = futures::stream::once(async move {
                    let mut outturn = serde_json::Map::new();
                    if !pending.is_empty() {
                        outturn.insert("pending".into(), serde_json::json!(pending));
                    }
                    if announce.load(std::sync::atomic::Ordering::Relaxed) {
                        outturn.insert("cancelled".into(), serde_json::json!(true));
                    }
                    // Nothing to say is not worth a line: an empty envelope
                    // still costs a reader a parse. Decided on the map rather
                    // than on the length of what it serialises to, so that
                    // whether a cancel reaches the turn does not depend on how
                    // many spaces a serialiser happens to emit.
                    if outturn.is_empty() {
                        return None;
                    }
                    let line = serde_json::json!({ "outturn": outturn });
                    Some(Ok(axum::body::Bytes::from(format!("{line}\n"))))
                })
                .filter_map(std::future::ready);
                let body = body.chain(trailer);

                // Named so spend attaches to the endpoint that billed for
                // it, which failover makes different from the one configured
                // first.
                return Ok((
                    [
                        (
                            axum::http::header::CONTENT_TYPE,
                            "application/x-ndjson".to_string(),
                        ),
                        (
                            axum::http::HeaderName::from_static("x-outturn-provider"),
                            provider.endpoint(),
                        ),
                        // Whose credential paid. Every key is the operator's
                        // until workspaces can bring their own (docs/routing.md);
                        // the ledger carries the column from the start so the
                        // bill does not have to be re-derived when they can.
                        (
                            axum::http::HeaderName::from_static("x-outturn-paid-by"),
                            "operator".to_string(),
                        ),
                    ],
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
