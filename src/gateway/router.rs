use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use futures::StreamExt;

use crate::auth::{self, SessionClaims, TokenValidator};

use super::breaker;
use super::egress;
use super::llm::provider::{LlmProvider, ProviderError};
use super::llm::types::ChatCompletionRequest;
use super::routing::{self};

pub struct GatewayState {
    providers: Vec<Arc<dyn LlmProvider>>,
    auth: TokenValidator,
    /// Backs the shared circuit breaker and traffic routing. Optional so the
    /// gateway still runs without a database -- it falls back to the
    /// statically configured providers, which is what it did before either
    /// existed.
    health: Option<sqlx::postgres::PgPool>,
    providers_by_endpoint: routing::ProviderCache,
    /// Where a vetted outbound request physically leaves from. `Direct` here
    /// and in any open-source deployment; the seam exists because spreading
    /// egress across a pool of addresses, or routing it through an estate's
    /// own forward proxy, is somebody's infrastructure rather than ours.
    pub(crate) egress_transport: Arc<dyn egress::transport::EgressTransport>,
    /// Hosts inside the network an operator has opened, to agents and to
    /// routes alike. Empty unless one said otherwise, which leaves every
    /// private address refused as it was.
    pub(crate) internal: egress::internal::Internal,
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
            egress_transport: Arc::new(egress::transport::Direct),
            // Read once, at startup: an operator changing which internal hosts
            // are open is changing the deployment, and the list is read on
            // every outbound request.
            internal: egress::internal::Internal::from_env(),
        }
    }

    /// The internal hosts to treat as open, for a test that needs some.
    pub fn with_internal_hosts(mut self, internal: egress::internal::Internal) -> Self {
        self.internal = internal;
        self
    }

    /// Sends outbound requests some other way than straight out of this pod.
    ///
    /// Nothing in this repository calls it; it is what a deployment with its
    /// own egress estate replaces `Direct` with, and it exists so that doing
    /// so does not mean patching the path that decides what is allowed.
    pub fn with_egress_transport(
        mut self,
        transport: Arc<dyn egress::transport::EgressTransport>,
    ) -> Self {
        self.egress_transport = transport;
        self
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

    /// Why this session's work is being held, if it is.
    ///
    /// The other half of the kill switch. The guest checks at its round
    /// boundary, which is where a turn stops tidily -- but a boundary can be a
    /// whole completion away, and every token until then is spend past a cap
    /// that has already tripped. This cuts the stream instead.
    ///
    /// Fail-open, like every other thing the gateway cannot look up: a database
    /// it cannot reach stops nothing rather than stopping everything. That is
    /// the wrong way round for a spend cap and the right way round for an
    /// outage, and the turn-preparation check catches what this misses on the
    /// very next turn.
    async fn held_for(&self, workspace_id: uuid::Uuid, session_id: uuid::Uuid) -> Option<String> {
        let pool = self.health.as_ref()?;
        let held =
            match crate::api::inhibitor::postgres::covering_session(pool, workspace_id, session_id)
                .await
            {
                Ok(held) => held,
                Err(e) => {
                    tracing::warn!(error = %e, "could not check whether a turn was held");
                    return None;
                }
            };

        // Through `decide`, like every other checkpoint, so the reason a
        // mid-stream cut reports is the same one the next turn's refusal would
        // report. Two holds at once otherwise name one of them here and both
        // there, and whoever reads the stopped session releases the one they
        // were shown and wonders why it is still stopped.
        let decision = crate::api::inhibitor::decide(held);

        // Only a stop cuts a stream. A suspended turn is one that will be
        // picked up again, and cutting it mid-token is how a resumable turn
        // becomes a broken one.
        if decision.verdict != crate::api::inhibitor::Verdict::Stopped {
            return None;
        }
        Some(decision.why())
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

    /// Whether a route points somewhere this gateway may go.
    ///
    /// Public is fine, and private only where an operator opened it -- the same
    /// list the agent path consults, because "may the gateway reach this host"
    /// is one question however it came to be asked.
    ///
    /// A route naming something unparseable, or a host that does not resolve,
    /// is dropped rather than refused: it was already going to fail, and
    /// failing here says why once instead of on every request.
    fn route_is_reachable(&self, route: &routing::Route) -> bool {
        let Ok(url) = reqwest::Url::parse(&route.base_url) else {
            tracing::warn!(base_url = %route.base_url, "a route's base URL is not a URL; skipping it");
            return false;
        };
        let Some(host) = url.host_str() else {
            tracing::warn!(base_url = %route.base_url, "a route's base URL names no host; skipping it");
            return false;
        };
        let Some(port) = url.port_or_known_default() else {
            return false;
        };

        // Opened settles it, by name or by address, which is the case a
        // deliberately configured internal route takes.
        if self.internal.allows(host, port) {
            return true;
        }

        // Otherwise only a literal address is judged here, and a name is left
        // alone. Resolving on this path was tried and is not affordable: a name
        // that does not resolve costs the resolver's full timeout -- four
        // seconds, measured -- and this runs per route per request, so one
        // stale route would stall every turn in the deployment.
        //
        // What that leaves open is a route naming one of our own services by
        // hostname. It is a narrower hole than it looks: routes are written by
        // whoever runs the deployment, `traffic_routes` has no write API, and
        // the thing it would buy an attacker is reaching an API that requires a
        // token the gateway will not mint for them. Worth closing when routes
        // become workspace-writable, and worth closing then by giving the
        // gateway its siblings' names rather than by resolving.
        match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
            Ok(addr) if crate::runtime::egress::is_forbidden(addr) => {
                tracing::warn!(
                    base_url = %route.base_url,
                    "a route points inside the network and no operator opened that host; skipping it"
                );
                false
            }
            _ => true,
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
                        // A route's base URL was taken as given until now, so
                        // this path could reach anywhere the agent path could
                        // not. `traffic_routes` already has a nullable
                        // workspace_id, so workspace-owned routes are a shape
                        // the schema allows and only the absence of an API
                        // prevents -- vetting here means whoever builds that
                        // API does not have to notice.
                        if !self.route_is_reachable(&route) {
                            continue;
                        }
                        if let Some(provider) = self.providers_by_endpoint.get(&route).await {
                            attempts.push(Attempt {
                                provider,
                                model: Some(route.model),
                            });
                        }
                    }
                    // Returned as it stands, empty or not. Falling through to
                    // the static providers here looks like resilience and is
                    // the opposite: a route was configured and rejected, and
                    // the static list is a *different vendor* with a different
                    // model. A deployment confined to an in-cluster provider
                    // whose host stops being allowed would quietly start
                    // sending its prompts to a public API -- and a route whose
                    // credential is momentarily missing would silently
                    // downgrade the model mid-conversation. Refusing is the
                    // answer that can be noticed and fixed; the fix for an
                    // in-cluster route being skipped is to name its host in
                    // OUTTURN_INTERNAL_HOSTS, which the warning above says.
                    //
                    // Falling through stays what it always was: for a traffic
                    // type nobody configured, which is the `Ok(_)` arm below.
                    if attempts.is_empty() {
                        tracing::error!(
                            traffic_type,
                            "every configured route was rejected; this traffic type cannot be served"
                        );
                    }
                    return attempts;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "could not read routes, using static providers")
                }
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
    ///
    /// A provider the platform configured is one circuit for everybody, which
    /// is what `None` says: the credential and the rate limit behind it are
    /// shared, so the failures are too. A workspace bringing its own key would
    /// name itself here and get a circuit of its own.
    async fn admits(&self, provider: &Arc<dyn LlmProvider>) -> bool {
        let Some(pool) = &self.health else {
            return true;
        };
        breaker::check(pool, &provider.endpoint(), None).await == breaker::Verdict::Allow
    }

    /// Feeds the outcome of a call back into the shared breaker.
    ///
    /// The caller travels with it because what an answered failure means
    /// depends on how many distinct callers are seeing it -- one caller's bad
    /// request and an upstream that is down look identical from a count.
    async fn observe(
        &self,
        provider: &Arc<dyn LlmProvider>,
        caller: breaker::policy::Caller,
        outcome: Result<(), &ProviderError>,
    ) {
        let Some(pool) = &self.health else {
            return;
        };
        let endpoint = provider.endpoint();
        let (observation, detail) = match outcome {
            Ok(()) => (breaker::policy::Observation::Success, None),
            Err(e) => (breaker::observation_for(e), Some(e.to_string())),
        };
        breaker::observe(
            pool,
            &endpoint,
            None,
            observation,
            caller,
            detail.as_deref(),
        )
        .await;
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

pub(crate) fn authenticate(
    state: &GatewayState,
    headers: &axum::http::HeaderMap,
) -> Result<SessionClaims, (StatusCode, String)> {
    let token =
        auth::extract_bearer(headers).map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    let claims = state.auth.validate(token).map_err(|e| match e {
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
) -> Result<axum::response::Response, (StatusCode, String)> {
    let claims = authenticate(&state, &headers)?;

    tracing::debug!(
        session_id = %claims.subject,
        workspace_id = %claims.workspace_id,
        model = %request.model,
        "chat completion request"
    );

    // Who is asking, for the breaker: a turn token names the workspace and the
    // session, and breadth is counted over one or the other depending on whose
    // credential the circuit is about.
    let caller = breaker::policy::Caller {
        workspace_id: claims.workspace_id,
        session_id: claims.subject,
    };

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
                state.observe(provider, caller, Ok(())).await;
                // The same two the streamed path sets. A caller that does not
                // stream still has to write a ledger row, and it cannot name
                // the endpoint it reached or who paid for it from the body.
                return Ok((
                    [
                        (
                            axum::http::HeaderName::from_static("x-outturn-provider"),
                            provider.endpoint(),
                        ),
                        (
                            axum::http::HeaderName::from_static("x-outturn-paid-by"),
                            "operator".to_string(),
                        ),
                    ],
                    Json(response),
                )
                    .into_response());
            }
            Err(e) => {
                tracing::warn!(provider = ?provider.provider(), error = %e, "provider failed");
                state.observe(provider, caller, Err(&e)).await;
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

    // As above: breadth is counted over callers, so the breaker has to be told
    // which one this is.
    let caller = breaker::policy::Caller {
        workspace_id: claims.workspace_id,
        session_id: claims.subject,
    };

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
                state.observe(provider, caller, Ok(())).await;
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
                // Why it stopped, where a hold rather than a person stopped it.
                // Carried to the turn so the transcript can say more than that
                // the words ceased.
                let held = Arc::new(std::sync::Mutex::new(None::<String>));
                let noting = Arc::clone(&held);

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
                        // A hold cuts the stream the same way a person pressing
                        // stop does. The turn is told which it was, because
                        // "you were stopped" and "the workspace was stopped"
                        // are different things to say afterwards.
                        if let Some(reason) = stopper.held_for(workspace_id, session_id).await {
                            if let Ok(mut slot) = noting.lock() {
                                *slot = Some(reason);
                            }
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
                        // Present only where a hold did it, so a turn can tell
                        // a person pressing stop from a kill switch.
                        if let Some(reason) = held.lock().ok().and_then(|slot| slot.clone()) {
                            outturn.insert("held".into(), serde_json::json!(reason));
                        }
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
                state.observe(provider, caller, Err(&e)).await;
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
        // Outbound HTTP for an agent, made here because this is the tier that
        // holds credentials and the tier the runtime cannot bypass.
        .route("/v1/egress", post(super::egress::fetch))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::egress::internal::Internal;

    fn route(base_url: &str) -> routing::Route {
        routing::Route {
            provider: "openai".into(),
            base_url: base_url.into(),
            model: "m".into(),
            credential_ref: None,
        }
    }

    fn gateway(internal: &str) -> GatewayState {
        // A real validator over a throwaway key: nothing here presents a
        // token, and building one is cheaper than a seam for not having one.
        let validator = TokenValidator::with_keys(&[[7u8; 32]], crate::auth::AUDIENCE_GATEWAY)
            .expect("validator");
        GatewayState::new(Vec::new(), validator).with_internal_hosts(Internal::parse(internal))
    }

    /// A route pointed at the public internet is not this list's business.
    #[test]
    fn a_public_route_is_reachable_without_anybody_opening_it() {
        let state = gateway("");
        assert!(state.route_is_reachable(&route("https://api.openai.com")));
    }

    /// The hole this closes: routes were taken as given, so one pointed inside
    /// the network was reached while an agent asking for the same host was not.
    #[test]
    fn a_route_inside_the_network_is_skipped_unless_opened() {
        let shut = gateway("");
        assert!(!shut.route_is_reachable(&route("http://10.1.2.3:8000")));
        assert!(!shut.route_is_reachable(&route("http://[fd00::1]:8000")));
    }

    /// And the part it does not close, asserted so nobody believes otherwise.
    ///
    /// A route naming one of our own services by hostname is reached, because
    /// resolving to find out costs a resolver timeout on every request for
    /// every route that does not resolve. Narrow today -- routes have no write
    /// API and are written by whoever runs the deployment -- and the thing to
    /// close when they become workspace-writable, by telling the gateway its
    /// siblings' names rather than by looking them up.
    #[test]
    fn a_route_naming_one_of_our_own_services_is_not_caught() {
        let shut = gateway("");
        assert!(shut.route_is_reachable(&route("http://outturn-api:8080")));
    }

    #[test]
    fn an_opened_host_is_reachable_by_a_route() {
        let open = gateway("10.1.2.3:8000");
        assert!(open.route_is_reachable(&route("http://10.1.2.3:8000")));
    }

    /// The port narrows a route the same way it narrows an agent's request.
    #[test]
    fn opening_one_port_does_not_open_the_host() {
        let open = gateway("10.1.2.3:8000");
        assert!(!open.route_is_reachable(&route("http://10.1.2.3:9000")));
    }

    /// A name is left to the provider, and an operator can still open one.
    ///
    /// Resolving here was tried and costs a resolver timeout per route per
    /// request for a name that does not resolve, which one stale route would
    /// inflict on every turn. So what this catches is a literal address inside
    /// the network, which is the form a route configured by hand takes.
    #[test]
    fn a_name_is_left_to_the_provider() {
        let state = gateway("");
        assert!(state.route_is_reachable(&route("http://tickets.internal:8080")));

        let open = gateway("tickets.internal:8080");
        assert!(open.route_is_reachable(&route("http://tickets.internal:8080")));
    }

    #[test]
    fn a_route_that_is_not_a_url_is_dropped_rather_than_tried() {
        let state = gateway("");
        assert!(!state.route_is_reachable(&route("not a url")));
        assert!(!state.route_is_reachable(&route("http://")));
    }
}
