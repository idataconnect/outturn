use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use futures::StreamExt;

use outturn_core::auth::{self, SessionClaims, TokenValidator};

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
    pub(crate) health: Option<sqlx::postgres::PgPool>,
    providers_by_account: routing::ProviderCache,
    /// Where a vetted outbound request physically leaves from. `Direct` here
    /// and in any open-source deployment; the seam exists because spreading
    /// egress across a pool of addresses, or routing it through an estate's
    /// own forward proxy, is somebody's infrastructure rather than ours.
    pub(crate) egress_transport: Arc<dyn egress::transport::EgressTransport>,
    /// Hosts inside the network an operator has opened, to agents and to
    /// routes alike. Empty unless one said otherwise, which leaves every
    /// private address refused as it was.
    pub(crate) internal: egress::internal::Internal,
    /// Access tokens for rules that exchange client credentials, held in this
    /// replica's memory and nowhere else. See `egress::client`.
    pub(crate) client_tokens: egress::client::Tokens,
    /// Which workspaces may use each credential variable, and where it may
    /// go. The operator's, beside the secrets themselves, so neither a
    /// workspace's rule nor the API decides where a secret is sent.
    pub(crate) bindings: outturn_core::egress::bindings::Bindings,
    /// Sealed credentials, read by id and opened with this gateway's key. See
    /// docs/sealed-credentials.md.
    pub(crate) sealed: egress::sealed::Credentials,
}

/// One thing to try: a provider, and the model to ask it for.
///
/// The model is carried alongside because a route decides it. When routing is
/// not configured the caller's own choice stands, which is how this behaved
/// before traffic types existed.
struct Attempt {
    provider: Arc<dyn LlmProvider>,
    model: Option<String>,
    /// What the route's model accepts; none for the caller's own choice,
    /// which nobody described.
    facts: Option<Arc<outturn_core::facts::ModelFacts>>,
    /// The configured model, for its settings; none for the caller's choice.
    model_id: Option<uuid::Uuid>,
}

impl GatewayState {
    pub fn new(providers: Vec<Arc<dyn LlmProvider>>, auth: TokenValidator) -> Self {
        Self {
            providers,
            auth,
            health: None,
            providers_by_account: routing::ProviderCache::default(),
            egress_transport: Arc::new(egress::transport::Direct),
            // Read once, at startup: an operator changing which internal hosts
            // are open is changing the deployment, and the list is read on
            // every outbound request.
            internal: egress::internal::Internal::from_env(),
            client_tokens: egress::client::Tokens::default(),
            // Read once, like the internal hosts, and for the same reason.
            bindings: outturn_core::egress::bindings::Bindings::from_env(),
            sealed: egress::sealed::Credentials::from_env(),
        }
    }

    /// Sealed credentials read from this database, for a test or a gateway
    /// that has one. `with_health` does this too.
    pub fn with_sealed(mut self, sealed: egress::sealed::Credentials) -> Self {
        self.sealed = sealed;
        self
    }

    /// The internal hosts to treat as open, for a test that needs some.
    pub fn with_internal_hosts(mut self, internal: egress::internal::Internal) -> Self {
        self.internal = internal;
        self
    }

    /// The credential bindings to enforce, for a test that needs some.
    pub fn with_bindings(mut self, bindings: outturn_core::egress::bindings::Bindings) -> Self {
        self.bindings = bindings;
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
        // The credentials table is in the same database, and a gateway with no
        // database attaches no sealed credential -- there is nothing to read
        // one from, so the request is refused rather than sent bare.
        self.sealed = std::mem::take(&mut self.sealed).with_pool(pool.clone());
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
        match outturn_core::jobs::live_turn_for_session(pool, workspace_id, session_id).await {
            Ok(Some(job_id)) => outturn_core::jobs::cancel_requested(pool, job_id)
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
        let held = match outturn_core::inhibitor::postgres::covering_session(
            pool,
            workspace_id,
            session_id,
        )
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
        let decision = outturn_core::inhibitor::decide(held);

        // Only a stop cuts a stream. A suspended turn is one that will be
        // picked up again, and cutting it mid-token is how a resumable turn
        // becomes a broken one.
        if decision.verdict != outturn_core::inhibitor::Verdict::Stopped {
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
                    if let Err(e) = outturn_core::events::append(
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
    #[cfg(test)]
    fn route_is_reachable(&self, route: &routing::Route) -> bool {
        self.route_turned_away(route).is_none()
    }

    /// A connection to an opened internal host that timed out, said with
    /// its likely cause. Opening a host in `OUTTURN_INTERNAL_HOSTS` passes
    /// this gateway's own check, and a network policy in front of the pod is
    /// a second gate that cannot explain itself: it drops the packets, and
    /// the only sign is a connection nothing answers. Anywhere else a timeout
    /// is said as it was.
    fn with_policy_hint(&self, endpoint: &str, e: ProviderError) -> ProviderError {
        let ProviderError::Unreached(detail) = &e else {
            return e;
        };
        if detail != "it did not answer in time" {
            return e;
        }
        let url = endpoint.split_once(':').map_or(endpoint, |(_, url)| url);
        let Some((host, port)) = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| Some((u.host_str()?.to_string(), u.port_or_known_default()?)))
        else {
            return e;
        };
        if !self.internal.allows(&host, port) {
            return e;
        }
        ProviderError::Unreached(format!(
            "{detail}. It is inside the network, so check that a network policy lets \
             the gateway reach it on port {port}"
        ))
    }

    /// Why a route may not be called, in a sentence for whoever's turn it
    /// failed; none when it may.
    fn route_turned_away(&self, route: &routing::Route) -> Option<String> {
        let Ok(url) = reqwest::Url::parse(&route.base_url) else {
            tracing::warn!(base_url = %route.base_url, "a route's base URL is not a URL; skipping it");
            return Some(format!("{} is not a URL", route.base_url));
        };
        let Some(host) = url.host_str() else {
            tracing::warn!(base_url = %route.base_url, "a route's base URL names no host; skipping it");
            return Some(format!("{} names no host", route.base_url));
        };
        let Some(port) = url.port_or_known_default() else {
            return Some(format!("{} names no port", route.base_url));
        };

        // Opened settles it, by name or by address, which is the case a
        // deliberately configured internal route takes.
        if self.internal.allows(host, port) {
            return None;
        }

        // Otherwise only a literal address is judged here, and a name is left
        // alone. Resolving on this path was tried and is not affordable: a name
        // that does not resolve costs the resolver's full timeout -- four
        // seconds, measured -- and this runs per route per request, so one
        // stale route would stall every turn in the deployment.
        //
        // What that leaves open is a route naming one of our own services by
        // hostname. It is a narrower hole than it looks: routes are written by
        // whoever runs the deployment, chains have no write API yet, and
        // the thing it would buy an attacker is reaching an API that requires a
        // token the gateway will not mint for them. Worth closing when routes
        // become workspace-writable, and worth closing then by giving the
        // gateway its siblings' names rather than by resolving.
        match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
            Ok(addr) if outturn_core::egress::rule::is_forbidden(addr) => {
                tracing::warn!(
                    base_url = %route.base_url,
                    "a route points inside the network and no operator opened that host; skipping it"
                );
                Some(format!(
                    "{} is inside the network, and the gateway reaches such an address only where \
                     OUTTURN_INTERNAL_HOSTS opens it",
                    route.base_url
                ))
            }
            _ => None,
        }
    }

    /// What to try, in order of precedence.
    ///
    /// A configured route list wins. With none -- no database, or nothing
    /// configured for this traffic type -- the statically built providers are
    /// used in their configured order, so an unconfigured system behaves as it
    /// always has rather than refusing to serve.
    async fn attempts(
        &self,
        workspace_id: uuid::Uuid,
        traffic_type: &str,
        began: Option<chrono::DateTime<chrono::Utc>>,
    ) -> (Vec<Attempt>, Vec<String>) {
        // Why each configured route was turned away, for the message when
        // none is left: a chain that exists and was refused is not a class
        // with no chain, and saying so sent somebody to configure what they
        // already had.
        let mut turned_away = Vec::new();
        if let Some(pool) = &self.health {
            match routing::routes_for_turn(pool, workspace_id, traffic_type, began).await {
                Ok(routes) if !routes.is_empty() => {
                    let mut attempts = Vec::with_capacity(routes.len());
                    for route in routes {
                        // A route's base URL was taken as given until now, so
                        // this path could reach anywhere the agent path could
                        // not. Accounts already have a nullable workspace_id,
                        // so workspace-owned ones are a shape the schema
                        // allows -- vetting here means whoever builds their
                        // API does not have to notice.
                        if let Some(why) = self.route_turned_away(&route) {
                            turned_away.push(why);
                            continue;
                        }
                        if let Some(provider) = self
                            .providers_by_account
                            .get(&route, &self.sealed, workspace_id)
                            .await
                        {
                            attempts.push(Attempt {
                                provider,
                                model: Some(route.model),
                                facts: Some(route.facts),
                                model_id: Some(route.model_id),
                            });
                        } else {
                            turned_away.push(format!(
                                "{} at {} has no key the gateway can use",
                                route.model, route.base_url
                            ));
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
                    return (attempts, turned_away);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "could not read routes, using static providers")
                }
            }
        }

        let fallback = self
            .providers
            .iter()
            .map(|p| Attempt {
                provider: Arc::clone(p),
                model: None,
                facts: None,
                model_id: None,
            })
            .collect();
        (fallback, turned_away)
    }

    /// Fills in what the caller left to the settings: the temperature and
    /// effort chosen for this attempt's model, as the turn's agent has them
    /// (docs/models.md). Here, per attempt, because a fallback is asked with
    /// its own model's settings, never the first's. What the caller named
    /// itself -- a guest's own temperature, the namer's "none" -- stands.
    ///
    /// The agent is looked up from the session the token names, not taken
    /// from anything the runtime sent. With no database the request goes as
    /// it came.
    async fn apply_settings(
        &self,
        request: &mut ChatCompletionRequest,
        claims: &SessionClaims,
        model_id: Option<uuid::Uuid>,
    ) {
        let Some(pool) = &self.health else {
            return;
        };
        if request.temperature.is_some() && request.reasoning_effort.is_some() {
            return;
        }
        let agent: Option<uuid::Uuid> = sqlx::query_scalar(
            "select agent_id from agent_sessions where id = $1 and workspace_id = $2",
        )
        .bind(claims.subject)
        .bind(claims.workspace_id)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not read the session's agent; its own settings are not applied");
            None
        });
        let facts = request.facts.as_deref().cloned().unwrap_or_default();
        match outturn_core::model_settings::resolve(
            pool,
            claims.workspace_id,
            agent,
            model_id,
            &facts,
        )
        .await
        {
            Ok(settings) => {
                if request.temperature.is_none() {
                    request.temperature = settings.temperature;
                }
                if request.reasoning_effort.is_none() {
                    request.reasoning_effort = Some(settings.reasoning_effort);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not read settings; the request goes as it came")
            }
        }
    }

    /// Whether this provider may be called, and who owns the next probe.
    ///
    /// A provider the platform configured is one circuit for everybody, which
    /// is what `None` says: the credential and the rate limit behind it are
    /// shared, so the failures are too. A workspace bringing its own key would
    /// name itself here and get a circuit of its own.
    async fn admits(&self, provider: &Arc<dyn LlmProvider>, model: &str) -> bool {
        let Some(pool) = &self.health else {
            return true;
        };
        breaker::check(pool, &provider.endpoint(), None).await == breaker::Verdict::Allow
            && breaker::check(pool, &model_circuit(provider, model), None).await
                == breaker::Verdict::Allow
    }

    /// Feeds the outcome of a call back into the shared breaker.
    ///
    /// The caller travels with it because what an answered failure means
    /// depends on how many distinct callers are seeing it -- one caller's bad
    /// request and an upstream that is down look identical from a count.
    async fn observe(
        &self,
        provider: &Arc<dyn LlmProvider>,
        model: &str,
        caller: breaker::policy::Caller,
        outcome: Result<(), &ProviderError>,
    ) {
        let Some(pool) = &self.health else {
            return;
        };
        // A model that answered is one that exists, and a model that is gone
        // is that model's circuit and no other's: the endpoint goes on serving
        // everything else it has.
        let model_outcome = match outcome {
            Ok(()) => Some(breaker::policy::Observation::Success),
            Err(e) if e.is_model_gone() => Some(breaker::policy::Observation::Retired),
            Err(_) => None,
        };
        if let Some(observation) = model_outcome {
            if observation == breaker::policy::Observation::Retired {
                tracing::warn!(
                    endpoint = %provider.endpoint(),
                    model,
                    "the model is gone from this endpoint; skipping it for a day"
                );
            }
            breaker::observe(
                pool,
                &model_circuit(provider, model),
                None,
                observation,
                caller,
                outcome.err().map(|e| e.to_string()).as_deref(),
            )
            .await;
        }
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

/// The circuit a model's existence at an endpoint is kept under: the endpoint
/// and the model together, so a retired model takes only its own route out.
fn model_circuit(provider: &Arc<dyn LlmProvider>, model: &str) -> String {
    format!("{} model={model}", provider.endpoint())
}

/// The reply the caller is writing, so a message taken mid-turn can name what
/// absorbed it.
const REPLY_HEADER: &str = "x-outturn-reply";

/// The model a chain tried first, and why it was passed over, so a reply
/// answered by another can say so (`reply_fallbacks`).
#[derive(Default)]
struct Passed {
    /// The model tried first: its configured id, what it was asked for, and
    /// what model it is whichever door it was reached through.
    first: Option<(Option<uuid::Uuid>, String, Option<String>)>,
    why: Option<&'static str>,
    /// Why configured routes were not tried at all.
    turned_away: Vec<String>,
}

impl Passed {
    fn trying(&mut self, attempt: &Attempt, model: &str) {
        if self.first.is_none() {
            let canonical = attempt.facts.as_ref().and_then(|f| f.canonical.clone());
            self.first = Some((attempt.model_id, model.to_string(), canonical));
        }
    }

    /// Why a model was passed over, as a clause after its name. The first
    /// reason is the one kept: it is what moved the request off the first.
    fn because(&mut self, why: &'static str) {
        self.why.get_or_insert(why);
    }

    /// Records the change on the reply, when the model that answered is not
    /// the one tried first. Only on a reply of the token's own session: the
    /// reply id is the runtime's word, and a reply elsewhere is not its to
    /// annotate.
    async fn record(
        &self,
        state: &GatewayState,
        headers: &axum::http::HeaderMap,
        session: uuid::Uuid,
        served: &Attempt,
        model: &str,
    ) {
        let (Some(pool), Some(reply), Some((first_id, first, first_canonical))) =
            (&state.health, reply_id(headers), &self.first)
        else {
            return;
        };
        // The same model through another door is not a change of model:
        // Sonnet on Bedrock standing in for Sonnet at Anthropic answers as
        // Sonnet. Said by the catalog where it knows both, else by name.
        let served_canonical = served.facts.as_ref().and_then(|f| f.canonical.as_deref());
        let same = match (first_canonical.as_deref(), served_canonical) {
            (Some(a), Some(b)) => a == b,
            _ => first == model,
        };
        if same {
            return;
        }
        let result = sqlx::query(
            "insert into reply_fallbacks (reply_id, session_id, configured_model, configured_name, \
                 served_model, served_name, reason) \
             select $1, $2, $3, (select name from models where id = $4), \
                    $5, (select name from models where id = $6), $7 \
             where exists (select 1 from agent_messages where id = $1 and session_id = $2) \
             on conflict (reply_id) do nothing",
        )
        .bind(reply)
        .bind(session)
        .bind(first)
        .bind(*first_id)
        .bind(model)
        .bind(served.model_id)
        .bind(self.why.unwrap_or("was unavailable"))
        .execute(pool)
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "could not record that a reply changed model");
        }
    }
}

/// Why a model's failure moved the request along, as a clause.
fn passed_because(e: &ProviderError) -> &'static str {
    if e.is_model_gone() {
        "is no longer offered"
    } else if e.may_have_been_taken() {
        "was sent the request and its answer was lost, so it was not sent again"
    } else if matches!(e, ProviderError::RateLimited)
        || matches!(
            e,
            ProviderError::Upstream {
                status: Some(429),
                ..
            }
        )
    {
        "was rate limited"
    } else {
        "was unavailable"
    }
}

fn reply_id(headers: &axum::http::HeaderMap) -> Option<uuid::Uuid> {
    headers
        .get(REPLY_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

/// When the turn this call belongs to began: the reply it is writing was
/// made then, and its id is a UUIDv7, which says when. Forged, it can only
/// choose the version a model was on before its last move -- one the
/// operator configured -- so it is taken as given.
fn turn_began(headers: &axum::http::HeaderMap) -> Option<chrono::DateTime<chrono::Utc>> {
    let (secs, nanos) = reply_id(headers)?.get_timestamp()?.to_unix();
    chrono::DateTime::from_timestamp(i64::try_from(secs).ok()?, nanos)
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
    authenticate_for(state, headers, auth::Authority::GatewayInvoke)
}

/// As `authenticate`, for a route that something other than a turn may call.
pub(crate) fn authenticate_for(
    state: &GatewayState,
    headers: &axum::http::HeaderMap,
    authority: auth::Authority,
) -> Result<SessionClaims, (StatusCode, String)> {
    let token =
        auth::extract_bearer(headers).map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    let claims = state.auth.validate(token).map_err(|e| match e {
        auth::AuthError::Forbidden => (StatusCode::FORBIDDEN, e.to_string()),
        _ => (StatusCode::UNAUTHORIZED, e.to_string()),
    })?;
    // The gateway has no role store and needs none: the only roles a token it
    // accepts can carry are the platform's `turn` and `document_fetch`.
    claims
        .require_platform(authority)
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

    let mut passed = Passed::default();
    let (attempts, turned_away) = state
        .attempts(claims.workspace_id, &traffic, turn_began(&headers))
        .await;
    passed.turned_away = turned_away;
    for attempt in attempts {
        let provider = &attempt.provider;
        let model = attempt
            .model
            .as_deref()
            .unwrap_or(&request.model)
            .to_string();
        passed.trying(&attempt, &model);
        if !state.admits(provider, &model).await {
            passed.because("has been failing, and is being rested");
            continue;
        }

        // The route decides the model when there is one; otherwise the
        // caller's choice stands.
        let mut request = request.clone();
        if let Some(model) = &attempt.model {
            request.model = model.clone();
        }
        request.facts = attempt.facts.clone();
        state
            .apply_settings(&mut request, &claims, attempt.model_id)
            .await;

        match provider.chat_completion(&request).await {
            Ok(response) => {
                state.observe(provider, &model, caller, Ok(())).await;
                passed
                    .record(&state, &headers, claims.subject, &attempt, &model)
                    .await;
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
                let e = state.with_policy_hint(&provider.endpoint(), e);
                tracing::warn!(provider = ?provider.provider(), endpoint = %provider.endpoint(), error = %e, "provider failed");
                state.observe(provider, &model, caller, Err(&e)).await;
                // A client error is the request being wrong rather than the
                // endpoint being unwell, so the next route refuses it the same
                // way -- walking the list spends attempts to reach the same
                // answer and buries the one message that explains it. 408 and
                // 429 are excluded from that reading, so a provider asking to
                // be retried still is.
                // A model that is gone is this route's failure, not the
                // request's: the next route may have one that is not.
                let fatal = e.is_client_error() && !e.is_model_gone();
                // Sent, and the answer lost: the provider may have done the
                // work and billed for it, and the next route would do it
                // again. Said loudly, since nothing else records the attempt.
                let taken = e.may_have_been_taken();
                if taken {
                    tracing::warn!(
                        provider = ?provider.provider(),
                        endpoint = %provider.endpoint(),
                        model = %model,
                        workspace_id = %claims.workspace_id,
                        "a request was sent and its answer lost; it may have been billed, \
                         so it is not sent to another route"
                    );
                }
                passed.because(passed_because(&e));
                last_error = Some(e);
                if fatal || taken {
                    break;
                }
                continue;
            }
        }
    }

    // The same sentence the streaming route gives, for the same reader.
    let msg = unanswered(&passed, last_error.as_ref(), &traffic);
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
) -> Response {
    let claims = match authenticate(&state, &headers) {
        Ok(claims) => claims,
        Err(refused) => return refused.into_response(),
    };

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

    // Asked on a tick for as long as this request lives, from before the
    // provider is called: a local model can spend minutes on a long prompt
    // before its first byte, and a stop that waited for one would wait that
    // long. Holds `Some` once stopped -- with the hold's reason where a hold
    // rather than a person did it, carried to the turn so the transcript can
    // say more than that the words ceased.
    //
    // Per request, not shared: two sessions streaming at once must not be
    // able to stop one another. The task ends when nothing is listening.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(None::<Option<String>>);
    {
        let stopper = Arc::clone(&state);
        let workspace_id = claims.workspace_id;
        let session_id = claims.subject;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(CANCEL_POLL).await;
                if stop_tx.is_closed() {
                    return;
                }
                if stopper.cancel_requested(workspace_id, session_id).await {
                    let _ = stop_tx.send(Some(None));
                    return;
                }
                // A hold cuts the stream the same way a person pressing stop
                // does. The turn is told which it was, because "you were
                // stopped" and "the workspace was stopped" are different
                // things to say afterwards.
                if let Some(reason) = stopper.held_for(workspace_id, session_id).await {
                    let _ = stop_tx.send(Some(Some(reason)));
                    return;
                }
            }
        });
    }
    let stopped = |rx: &tokio::sync::watch::Receiver<Option<Option<String>>>| {
        let mut rx = rx.clone();
        async move {
            // A closed channel without a stop means nobody will ever send one.
            if rx.wait_for(|v| v.is_some()).await.is_err() {
                futures::future::pending::<()>().await;
            }
        }
    };

    // Appended after the provider's chunks, in the same framing. A caller that
    // does not know this line exists ignores it, so an older runtime keeps
    // working -- it simply does not steer.
    //
    // The cancel rides here rather than in a channel of its own for the same
    // reason: cutting the stream stops the words, but only this tells the turn
    // *why* they stopped, which is the difference between ending deliberately
    // and looking like a provider that hung up.
    let trailer = |pending: Vec<routing::Pending>,
                   stop: Option<Option<String>>|
     -> Option<axum::body::Bytes> {
        let mut outturn = serde_json::Map::new();
        if !pending.is_empty() {
            outturn.insert("pending".into(), serde_json::json!(pending));
        }
        if let Some(held) = stop {
            outturn.insert("cancelled".into(), serde_json::json!(true));
            // Present only where a hold did it, so a turn can tell a person
            // pressing stop from a kill switch.
            if let Some(reason) = held {
                outturn.insert("held".into(), serde_json::json!(reason));
            }
        }
        // Nothing to say is not worth a line: an empty envelope still costs a
        // reader a parse.
        if outturn.is_empty() {
            return None;
        }
        let line = serde_json::json!({ "outturn": outturn });
        Some(axum::body::Bytes::from(format!("{line}\n")))
    };

    // Named so spend attaches to the endpoint that billed for it, which
    // failover makes different from the one configured first.
    let respond = |endpoint: String, body: axum::body::Body| -> Response {
        (
            [
                (
                    axum::http::header::CONTENT_TYPE,
                    "application/x-ndjson".to_string(),
                ),
                (
                    axum::http::HeaderName::from_static("x-outturn-provider"),
                    endpoint,
                ),
                // Whose credential paid. Every key is the operator's until
                // workspaces can bring their own (docs/routing.md); the ledger
                // carries the column from the start so the bill does not have
                // to be re-derived when they can.
                (
                    axum::http::HeaderName::from_static("x-outturn-paid-by"),
                    "operator".to_string(),
                ),
            ],
            body,
        )
            .into_response()
    };

    let mut last_error = None;

    let mut passed = Passed::default();
    let (attempts, turned_away) = state
        .attempts(claims.workspace_id, &traffic, turn_began(&headers))
        .await;
    passed.turned_away = turned_away;
    for attempt in attempts {
        let provider = &attempt.provider;
        let model = attempt
            .model
            .as_deref()
            .unwrap_or(&request.model)
            .to_string();
        passed.trying(&attempt, &model);
        if !state.admits(provider, &model).await {
            passed.because("has been failing, and is being rested");
            continue;
        }

        let mut request = request.clone();
        if let Some(model) = &attempt.model {
            request.model = model.clone();
        }
        request.facts = attempt.facts.clone();
        state
            .apply_settings(&mut request, &claims, attempt.model_id)
            .await;

        // Dropping the call is the stop: it closes the connection, which is
        // the only "never mind" a provider understands.
        let called = tokio::select! {
            called = provider.chat_completion_stream(&request) => called,
            _ = stopped(&stop_rx) => {
                let stop = stop_rx.borrow().clone();
                let line = trailer(pending, stop);
                let body = futures::stream::iter(line.map(Ok::<_, std::io::Error>));
                return respond(
                    provider.endpoint(),
                    axum::body::Body::from_stream(body),
                );
            }
        };

        match called {
            Ok(chunks) => {
                // Recorded once the provider has accepted and begun
                // streaming. A stream that dies partway is not seen here --
                // the body is handed to the caller and this scope ends -- so
                // the breaker measures reachability, not completion.
                state.observe(provider, &model, caller, Ok(())).await;
                passed
                    .record(&state, &headers, claims.subject, &attempt, &model)
                    .await;
                let body = chunks.map(|chunk| match chunk {
                    Ok(chunk) => serde_json::to_string(&chunk)
                        .map(|mut line| {
                            line.push('\n');
                            axum::body::Bytes::from(line)
                        })
                        .map_err(|e| std::io::Error::other(e.to_string())),
                    Err(e) => Err(std::io::Error::other(e.to_string())),
                });

                // Ends the moment a stop is seen, whether or not a chunk is
                // arriving -- a provider gone quiet is exactly when somebody
                // presses stop. Ending the stream drops the provider's
                // response body and closes the connection it was arriving on.
                //
                // Best-effort by nature. A provider may finish generating and
                // bill for it regardless, and nothing here can find out.
                let body = body.take_until(stopped(&stop_rx));

                let rx = stop_rx.clone();
                let tail = futures::stream::once(async move {
                    let stop = rx.borrow().clone();
                    trailer(pending, stop).map(Ok)
                })
                .filter_map(std::future::ready);
                let body = body.chain(tail);

                return respond(provider.endpoint(), axum::body::Body::from_stream(body));
            }
            Err(e) => {
                let e = state.with_policy_hint(&provider.endpoint(), e);
                // The Debug form, whole: what a caller is shown is capped.
                tracing::warn!(provider = ?provider.provider(), endpoint = %provider.endpoint(), error = ?e, "stream failed");
                state.observe(provider, &model, caller, Err(&e)).await;
                // A model that is gone is this route's failure, not the
                // request's: the next route may have one that is not.
                let fatal = e.is_client_error() && !e.is_model_gone();
                // Sent, and the answer lost: the provider may have done the
                // work and billed for it, and the next route would do it
                // again. Said loudly, since nothing else records the attempt.
                let taken = e.may_have_been_taken();
                if taken {
                    tracing::warn!(
                        provider = ?provider.provider(),
                        endpoint = %provider.endpoint(),
                        model = %model,
                        workspace_id = %claims.workspace_id,
                        "a request was sent and its answer lost; it may have been billed, \
                         so it is not sent to another route"
                    );
                }
                passed.because(passed_because(&e));
                last_error = Some(e);
                if fatal || taken {
                    break;
                }
                continue;
            }
        }
    }

    // Says which failure ended it rather than only that one did. "no provider
    // could stream" is true of a misconfigured route, an outage and a
    // malformed request alike, and an operator reading it learns none of them
    // -- the reason was in the gateway's log and nowhere a caller could see.
    //
    // A reply the model wrote but nobody can use is marked as such, so the
    // runtime can tell it from a provider that is down: retrying an outage
    // may work, but retrying a model that looped repeats the minutes it
    // spent looping.
    let malformed = last_error.as_ref().is_some_and(|e| e.is_malformed_reply());
    // The last failure's own sentence, capitalized, and nothing around it:
    // this is what the person whose turn failed is shown. "No provider could
    // stream" said only that something had, which every failure here does.
    let message = unanswered(&passed, last_error.as_ref(), &traffic);
    if malformed {
        return (
            StatusCode::BAD_GATEWAY,
            [(
                axum::http::HeaderName::from_static(FAILURE_HEADER),
                MALFORMED_REPLY,
            )],
            message,
        )
            .into_response();
    }
    (StatusCode::BAD_GATEWAY, message).into_response()
}

/// What a caller is told when no provider answered: the last failure's own
/// sentence, or that there was nobody to ask. Public so a test standing in
/// for the gateway says exactly what the gateway says.
/// Why nothing answered: nothing was configured to, or what failed last.
///
/// Nothing tried means no chain answers the class -- or none of its models
/// could be reached at all -- and there is no other answer: the gateway no
/// longer keeps a provider of its own to fall back to, since one nobody
/// chose would answer every unconfigured class silently.
fn unanswered(passed: &Passed, last_error: Option<&ProviderError>, traffic: &str) -> String {
    if passed.first.is_none() && last_error.is_none() {
        if !passed.turned_away.is_empty() {
            return format!(
                "Every model configured for {traffic} traffic was turned away: {}.",
                passed.turned_away.join("; ")
            );
        }
        return format!(
            "No model is configured for {traffic} traffic. The operator can give it a chain on \
             Platform, Models."
        );
    }
    failure_message(last_error)
}

pub fn failure_message(last_error: Option<&ProviderError>) -> String {
    match last_error {
        Some(e) => sentence(&e.to_string()),
        None => "No model provider was available for this request.".into(),
    }
}

/// A message as a sentence: capitalized, and ended if it was not already.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out: String = first.to_uppercase().chain(chars).collect();
    if !out.ends_with(['.', '!', '?', ')']) {
        out.push('.');
    }
    out
}

use outturn_core::protocol::{FAILURE_HEADER, MALFORMED_REPLY, TRAFFIC_HEADER};

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
    use outturn_core::egress::internal::Internal;

    fn route(base_url: &str) -> routing::Route {
        routing::Route {
            model_id: uuid::Uuid::nil(),
            account: uuid::Uuid::nil(),
            credential_id: None,
            provider: "openai".into(),
            base_url: base_url.into(),
            model: "m".into(),
            facts: Default::default(),
            credential_ref: None,
        }
    }

    fn gateway(internal: &str) -> GatewayState {
        // A real validator over a throwaway key: nothing here presents a
        // token, and building one is cheaper than a seam for not having one.
        let validator =
            TokenValidator::with_keys(&[[7u8; 32]], outturn_core::auth::AUDIENCE_GATEWAY)
                .expect("validator");
        GatewayState::new(Vec::new(), validator).with_internal_hosts(Internal::parse(internal))
    }

    /// A timeout connecting to an opened internal host names the network
    /// policy, the gate that drops packets without a word; a public host's,
    /// or any other failure, is said as it was.
    #[test]
    fn an_internal_timeout_points_at_the_network_policy() {
        let state = gateway("models.lan:11434");
        let timed_out = || ProviderError::Unreached("it did not answer in time".into());
        let said = state
            .with_policy_hint("openai:http://models.lan:11434", timed_out())
            .to_string();
        assert!(said.contains("network policy"), "{said}");
        assert!(said.contains("port 11434"), "{said}");

        let public = state
            .with_policy_hint("openai:https://api.example.com", timed_out())
            .to_string();
        assert!(!public.contains("network policy"), "{public}");
        let refused = state
            .with_policy_hint(
                "openai:http://models.lan:11434",
                ProviderError::Unreached("the connection was refused".into()),
            )
            .to_string();
        assert!(!refused.contains("network policy"), "{refused}");
    }

    /// A route pointed at the public internet is not this list's business.
    /// A chain whose every route was turned away says why, rather than that
    /// nothing is configured -- which sent somebody to configure the chain
    /// they already had.
    #[test]
    fn a_refused_chain_is_not_called_unconfigured() {
        let shut = gateway("");
        let mut passed = Passed::default();
        passed.turned_away.push(
            shut.route_turned_away(&route("http://172.18.0.1:11434"))
                .expect("a private address nobody opened"),
        );
        let said = unanswered(&passed, None, "assistant");
        assert!(said.contains("turned away"), "{said}");
        assert!(said.contains("OUTTURN_INTERNAL_HOSTS"), "{said}");
        let none = unanswered(&Passed::default(), None, "assistant");
        assert!(none.starts_with("No model is configured"), "{none}");
    }

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

#[cfg(test)]
mod sentence_tests {
    use super::sentence;

    #[test]
    fn a_reason_is_capitalized_and_ended_once() {
        assert_eq!(
            sentence(
                "the model provider refused the request: `temperature` is deprecated for this model."
            ),
            "The model provider refused the request: `temperature` is deprecated for this model."
        );
        assert_eq!(
            sentence("the model provider could not be reached: connection refused"),
            "The model provider could not be reached: connection refused."
        );
    }
}
