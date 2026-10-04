//! Outbound HTTP on a workspace's behalf, from the tier that holds credentials.
//!
//! This is the other half of `egress::commit`. The API commits to a turn's
//! egress rules and signs the commitment into the turn token; here is where
//! that signature finally does some work. The runtime presents the token and
//! the rule it wants to use, and the rule is checked against the root *inside
//! the token* rather than against anything the runtime supplied. A runtime
//! that rewrote its own copy of the rules gets nowhere, because the copy it
//! could rewrite is not the one consulted.
//!
//! Which is why the request is made from here rather than approved from here.
//! An approval the runtime is trusted to honour is worth nothing against a
//! runtime that has been compromised -- it would simply not ask. The tier that
//! runs workspace code has no outbound path of its own, so asking is the only
//! way out. (The cluster should say so too: a NetworkPolicy denying egress
//! from runtime pods makes that a property of the network rather than of the
//! runtime's good behavior.)
//!
//! The same move solves the credential. A legacy internal service wants its
//! own header with its own secret, and it is never going to verify one of our
//! signatures instead; so somebody has to hold that secret, and it must not be
//! the tier running workspace code. Here, the secret is resolved after the
//! rule is proven and attached after the guest's own headers, so nothing a
//! guest sent can displace it and nothing it can read contains it.
//!
//! What is *not* decided here is where the request physically leaves from.
//! See `transport`.

pub mod client;
pub mod internal;
pub mod sealed;
pub mod transport;

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};

use crate::egress::commit;
use crate::runtime::egress::{self as rules};

use transport::{TransportError, VettedRequest};

/// How long a request may take before it is abandoned.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How much of a response comes back.
const FETCH_BODY_LIMIT: usize = 256 * 1024;

/// How much of a document the API fetches comes back: as much as the OpenAPI
/// wizard will read, which is the only thing that asks. A turn's limit would
/// cut every real specification short.
const DOCUMENT_BODY_LIMIT: usize = crate::api::skill::wizard::MAX_SPEC_BYTES;

/// What the runtime asks for.
///
/// The rule and its proof travel together because the rule is not believed on
/// its own: what makes it usable is that it verifies against the commitment in
/// the caller's own token.
#[derive(Debug, Deserialize)]
pub struct EgressRequest {
    pub method: String,
    pub url: String,
    /// What the API said gates this turn, for checking against the commitment in
    /// the caller's own token.
    ///
    /// Sent as the whole set rather than one gate and a proof, because what has
    /// to be established is that this request matches *none* of them and a Merkle
    /// proof shows presence rather than absence. A set that does not hash to the
    /// token's root is refused, so a runtime that dropped a gate from its copy
    /// gets nowhere -- see `egress::gate`.
    #[serde(default = "crate::egress::gate::Gates::none")]
    pub gates: crate::egress::gate::Gates,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
    /// The rule the caller matched, and the proof that the API vouched for it.
    pub proof: commit::Proof,
}

/// What goes back, shaped like the response the guest will be handed.
#[derive(Debug, Serialize)]
pub struct EgressResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub truncated: bool,
}

/// Makes a request for a caller that has shown it is allowed to.
///
/// Every refusal comes back as plain text the model can read, because a tool
/// that fails opaquely is called again the same way. None of them say anything
/// the model did not already have: that a host is not allowed is a fact about
/// the workspace's settings, and that a name resolves inside the cluster is
/// something it had to guess to ask.
pub async fn fetch(
    State(state): State<Arc<super::GatewayState>>,
    headers: HeaderMap,
    Json(request): Json<EgressRequest>,
) -> Result<Json<EgressResponse>, (StatusCode, String)> {
    // The token is the whole basis of this. It says which workspace is calling
    // and what its rules hashed to, and the caller cannot write either.
    //
    // Either a turn, or the API fetching one document for an operator. The
    // second is told apart by its role and held to less: a GET, and nothing
    // a turn could not already ask for, but a response large enough to be a
    // real specification.
    let claims = super::router::authenticate(&state, &headers).or_else(|refused| {
        super::router::authenticate_for(&state, &headers, crate::auth::Authority::GatewayFetch)
            .map_err(|_| refused)
    })?;
    let document = !claims.has_platform_authority(crate::auth::Authority::GatewayInvoke);

    let committed = claims.egress_commitment().map_err(|_| {
        // A turn token with no commitment cannot be given the benefit of the
        // doubt: "absent" would otherwise be the most useful claim to strip.
        (
            StatusCode::FORBIDDEN,
            "this turn carries no egress commitment".to_string(),
        )
    })?;

    // The rules this workspace actually has, as vouched for by the API. The
    // caller sent them, but only those that verify against the token's root
    // survive -- so what comes back is the API's word, not the caller's.
    let vouched = commit::verify(claims.workspace_id, &committed, &request.proof)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    // Against the one list, in `egress::gate`, because that is what
    // `for_unreviewed_hosts` fans a host gate across: a verb this tier would send
    // and no gate covers is an un-approved request, silently.
    let method = request.method.to_ascii_uppercase();
    if !crate::egress::gate::METHODS.contains(&method.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{method} is not a method this can send"),
        ));
    }

    if document && method != "GET" {
        return Err((
            StatusCode::FORBIDDEN,
            "a document is fetched, never sent to".to_string(),
        ));
    }

    let url = reqwest::Url::parse(&request.url)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("that URL is not one: {e}")))?;

    // Matched against the vouched rules rather than the sent ones: the host
    // has to be allowed by a rule the API committed to, and the rule that
    // matches is the one whose credential travels.
    let (host, rule) =
        rules::check_url(&vouched, &url).map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    // Whether this request is one somebody has to approve first.
    //
    // The gate set is checked against the commitment in the token before it is
    // consulted, and a token with no gate claim is refused outright. Both
    // directions matter and they are opposite to the egress check above: an
    // egress rule is a permission, so failing to prove one means refused and
    // absence is safe. A gate is an obligation, so absence read as "nothing is
    // gated" would let every request through -- the answer a forger would choose.
    // Could-not-verify means refused here as there; what is refused is the
    // *request* rather than the rule.
    let gated = claims.gate_commitment().map_err(|_| {
        (
            StatusCode::FORBIDDEN,
            "this turn carries no statement about what needs approving".to_string(),
        )
    })?;
    if !request.gates.matches(claims.workspace_id, &gated) {
        return Err((
            StatusCode::FORBIDDEN,
            "the approval rules offered are not the ones this turn was given".to_string(),
        ));
    }
    // Every gate covering the request, not the first: each is an obligation, and
    // one satisfied says nothing about another. See `Gates::covering_all`.
    for gate in request.gates.covering_all(&host, &method, url.path()) {
        // Before anything is decided about approving it: a request missing a
        // field the declaration says its approval is keyed on cannot be
        // meaningfully approved, and must not become a differently-keyed request
        // that raises a second question nobody can answer.
        //
        // Named, because the guest is the only thing that can fix it and a bare
        // refusal would have it guessing.
        if let Some(field) =
            crate::egress::grant::missing_bound_field(gate, request.body.as_deref())
        {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "this request is missing `{field}`, which the approval for \"{}\" \
                     is keyed on. Send it as a top-level string or number and try again.",
                    gate.requires,
                ),
            ));
        }
    }
    if request.gates.covering(&host, &method, url.path()).is_some() {
        // Unless somebody has already approved this particular request, through
        // every gate that covers it. The
        // grant travels in the same commitment the gate does, so this is still
        // the API's word rather than the caller's, and still no database read in
        // this tier.
        //
        // Checked here rather than by leaving the gate out of the turn's
        // commitment, which is what this did first: a gate that is not committed
        // to is not enforced at all, so anything the omission was too broad about
        // became an ungated request. And a `unit` grant can only be checked where
        // the body is, which is here.
        if let Some(gate) =
            request
                .gates
                .unpermitted(&host, &method, url.path(), request.body.as_deref())
        {
            // Refused rather than held here. Parking the turn is the API's to do --
            // it owns the job and the queue -- and this tier has a method, a URL and a
            // token. What it can do is not make the call, and say why in words the
            // guest can read, which is what every other refusal here does.
            //
            // So the money does not move and get approved afterwards, which is the
            // ordering that matters.
            return Err((
                StatusCode::FORBIDDEN,
                format!(
                    "this needs approval before it can go out: {method} {} on {host} requires \"{}\"",
                    url.path(),
                    gate.requires,
                ),
            ));
        }
        tracing::info!(
            workspace_id = %claims.workspace_id,
            "a gated request was approved through every gate covering it, so it goes out"
        );
    }

    // A credential travels only where it cannot be read on the way. A
    // workspace that configured a key for a host did not consent to it going
    // out in clear because a model typed http.
    if (rule.credential_env.is_some() || rule.client.is_some() || rule.credential.is_some())
        && url.scheme() != "https"
    {
        return Err((
            StatusCode::FORBIDDEN,
            format!("{host} has a credential configured, so it can only be reached over https"),
        ));
    }

    let port = url.port_or_known_default().ok_or((
        StatusCode::BAD_REQUEST,
        "that URL names no port and its scheme implies none".to_string(),
    ))?;

    // The check a workspace cannot waive. An allowed name that resolves into
    // the cluster is still refused, and the answer is pinned so the check and
    // the connection are about the same place.
    //
    // Unless an operator opened this host. The refusal exists to stop a
    // workspace aiming the gateway at `outturn-api`, and it stops a customer's
    // own ticketing API for the same reason -- so the exceptions are named by
    // somebody outside the workspace. Resolution still happens and the address
    // is still pinned; only the judgement about the address is skipped, so what
    // was checked and what is connected to remain the same place.
    let addrs = if state.internal.allows(&host, port) {
        let addrs = rules::resolve(&host, port)
            .await
            .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
        tracing::debug!(%host, port, "reaching an internal host an operator opened");
        addrs
    } else {
        rules::resolve_and_vet(&host, port)
            .await
            .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?
    };

    let mut outgoing = reqwest::header::HeaderMap::new();
    for (name, value) in &request.headers {
        rules::check_header(name).map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
        let name: reqwest::header::HeaderName = name.parse().map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!("{name} is not a header name"),
            )
        })?;
        let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!("the {name} header's value cannot be sent"),
            )
        })?;
        outgoing.insert(name, value);
    }

    // Attached last, so nothing the caller sent can displace it, and read from
    // this tier's own environment -- the one place the runtime cannot reach.
    if let (Some(header), Some(variable)) = (&rule.header, &rule.credential_env) {
        // Again here, not only where the rule was written: a row written before
        // the namespace existed, or by hand, must not reach the operator's own.
        crate::runtime::egress::check_credential_variable(variable)
            .map_err(|why| (StatusCode::FORBIDDEN, why))?;
        // And bound to this workspace and this host by the operator, before it
        // is read: the namespace is shared, so naming a variable says nothing
        // about whose it is. The workspace is the token's, never the rule's.
        state
            .bindings
            .check_static(claims.workspace_id, variable, &host)
            .map_err(|why| (StatusCode::FORBIDDEN, why))?;
        let secret = std::env::var(variable).map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                format!("this host's credential ({variable}) is not configured"),
            )
        })?;
        let name: reqwest::header::HeaderName = header.parse().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{header} is not a header name"),
            )
        })?;
        let mut value = reqwest::header::HeaderValue::from_str(&secret).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "this host's credential cannot be sent as a header".to_string(),
            )
        })?;
        value.set_sensitive(true);
        outgoing.insert(name, value);
    }

    // A sealed credential: read by id from this tier's database, opened with
    // this tier's key, and sent only if what it was sealed to allows this
    // workspace -- the token's, never the rule's -- this host and this header.
    // The binding is the one the seal's tag covers, so nothing the API or the
    // database's writer changed afterwards can widen it.
    if let Some(id) = rule.credential {
        if rule.credential_env.is_some() || rule.client.is_some() {
            return Err((
                StatusCode::FORBIDDEN,
                format!("{host} has two credentials configured, so neither is sent"),
            ));
        }
        let header = rule.header.as_deref().ok_or((
            StatusCode::FORBIDDEN,
            format!("{host}'s credential has no header to travel in"),
        ))?;
        let opened = state
            .sealed
            .get(id)
            .await
            .map_err(|why| (StatusCode::FORBIDDEN, why))?;
        opened
            .binding
            .allows(
                claims.workspace_id,
                &host,
                Some(header),
                crate::egress::seal::Kind::Static,
            )
            .map_err(|why| (StatusCode::FORBIDDEN, why))?;
        let name: reqwest::header::HeaderName = header.parse().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{header} is not a header name"),
            )
        })?;
        let mut value = reqwest::header::HeaderValue::from_bytes(&opened.secret).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "this host's credential cannot be sent as a header".to_string(),
            )
        })?;
        value.set_sensitive(true);
        outgoing.insert(name, value);
    }

    // Exchanged here, after everything about the request itself has been
    // allowed, so a request that was going to be refused anyway spends nothing
    // at the token endpoint. Attached last for the reason the static one is.
    if let Some(client) = &rule.client {
        if rule.header.is_some() || rule.credential_env.is_some() {
            // The API refuses to write one, so this is a row edited by hand.
            return Err((
                StatusCode::FORBIDDEN,
                format!("{host} has two credentials configured, so neither is sent"),
            ));
        }
        // Before the cache as well as the exchange, so a token is handed out
        // only where the secret that bought it may go.
        state
            .bindings
            .check_client(claims.workspace_id, client, &host)
            .map_err(|why| (StatusCode::FORBIDDEN, why))?;
        let token = state
            .client_tokens
            .bearer(
                state.egress_transport.as_ref(),
                &state.internal,
                claims.workspace_id,
                rule,
                client,
            )
            .await
            .map_err(|e| {
                let status = if e.is_configuration() {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::BAD_GATEWAY
                };
                (status, e.to_string())
            })?;
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| {
                (
                    StatusCode::BAD_GATEWAY,
                    "this host's credential cannot be sent as a header".to_string(),
                )
            })?;
        value.set_sensitive(true);
        outgoing.insert(reqwest::header::AUTHORIZATION, value);
    }

    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "that is not a method this can send".to_string(),
        )
    })?;

    let vetted = VettedRequest {
        method,
        url,
        host: host.clone(),
        addrs,
        headers: outgoing,
        body: request.body,
        timeout: FETCH_TIMEOUT,
        body_limit: if document {
            DOCUMENT_BODY_LIMIT
        } else {
            FETCH_BODY_LIMIT
        },
    };

    match state.egress_transport.send(vetted).await {
        Ok(response) => {
            // A provider may revoke a token early, and nothing else would tell
            // the cache. Forgotten rather than retried: the 401 goes back as it
            // is, and the agent's next call exchanges afresh. Whether this
            // request is safe to send twice is not this tier's to decide -- see
            // docs/idempotency.md.
            if response.status == 401 && rule.client.is_some() {
                state.client_tokens.evict(claims.workspace_id, rule);
            }
            Ok(Json(EgressResponse {
                status: response.status,
                headers: response.headers,
                body: response.body,
                truncated: response.truncated,
            }))
        }
        // A failed request is not a refusal: the caller was allowed, and the
        // world did not cooperate. It comes back as text the model can act on
        // rather than as a status it cannot see.
        Err(e) => {
            let detail = e.to_string();
            match e {
                TransportError::Malformed(_) => Err((StatusCode::BAD_REQUEST, detail)),
                TransportError::Unreachable(_) | TransportError::Incomplete(_) => {
                    Err((StatusCode::BAD_GATEWAY, detail))
                }
            }
        }
    }
}

/// What a caller is allowed to reach, as far as this tier can tell.
///
/// Split out so the decision can be tested without a socket: everything up to
/// and including the credential is a function of the token, the proof and the
/// URL.
#[cfg(test)]
pub(crate) fn credential_for(rule: &crate::runtime::egress::EgressRule) -> Option<(&str, &str)> {
    match (&rule.header, &rule.credential_env) {
        (Some(header), Some(env)) => Some((header.as_str(), env.as_str())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::egress::EgressRule;

    fn rule(host: &str) -> EgressRule {
        EgressRule {
            host: host.into(),
            header: None,
            credential_env: None,
            client: None,
            credential: None,
        }
    }

    fn with_credential(host: &str, header: &str, env: &str) -> EgressRule {
        EgressRule {
            host: host.into(),
            header: Some(header.into()),
            credential_env: Some(env.into()),
            client: None,
            credential: None,
        }
    }

    #[test]
    fn a_rule_the_caller_made_up_does_not_verify_against_the_token() {
        // The whole point of doing this here rather than in the runtime: the
        // caller supplies the rules, and the root it must match is one only
        // the API could have signed.
        let workspace = uuid::Uuid::now_v7();
        let real = vec![rule("api.example.com")];
        let committed = commit::root(workspace, &real);

        let invented = commit::Proof::WholeSet {
            rules: vec![rule("evil.example.com")],
        };
        assert!(commit::verify(workspace, &committed, &invented).is_err());
    }

    #[test]
    fn only_the_vouched_rules_can_match_a_host() {
        // A caller cannot smuggle a host in beside a genuine one: the rules
        // matching is done against what verified, not against what was sent.
        let workspace = uuid::Uuid::now_v7();
        let real = vec![rule("api.example.com")];
        let committed = commit::root(workspace, &real);

        let proof = commit::Proof::WholeSet {
            rules: real.clone(),
        };
        let vouched = commit::verify(workspace, &committed, &proof).expect("verifies");

        let allowed = reqwest::Url::parse("https://api.example.com/things").expect("url");
        assert!(rules::check_url(&vouched, &allowed).is_ok());

        let smuggled = reqwest::Url::parse("https://evil.example.com/things").expect("url");
        assert!(rules::check_url(&vouched, &smuggled).is_err());
    }

    /// Allowing a name does not allow what the name resolves to.
    ///
    /// Moved here with the request it guards. A workspace can name `localhost`
    /// by accident or be talked into it; what stops the request is that the
    /// address behind the name is inside the network this runs in, and that is
    /// not the workspace's to waive. Driven through the same call the handler
    /// makes, so it fails if the handler ever stops making it.
    #[tokio::test]
    async fn an_allowed_name_that_resolves_inside_the_cluster_is_still_refused() {
        let vouched = vec![rule("localhost")];
        let url = reqwest::Url::parse("http://localhost:5432/").expect("url");
        let (host, _) = rules::check_url(&vouched, &url).expect("the rule allows the name");

        let refused = rules::resolve_and_vet(&host, 5432)
            .await
            .expect_err("an address inside the cluster must be refused");
        assert!(
            refused
                .to_string()
                .contains("cannot be reached from an agent"),
            "{refused}"
        );
    }

    /// A guest cannot aim the workspace's credential somewhere else.
    ///
    /// The header the credential travels in belongs to the platform. A guest
    /// that could set it could send the workspace's key to a host of its
    /// choosing, which is the leak the whole credential design is about.
    #[test]
    fn a_guest_cannot_set_the_headers_this_tier_owns() {
        for header in [
            "Authorization",
            "authorization",
            "COOKIE",
            "Host",
            "x-api-key",
        ] {
            assert!(rules::check_header(header).is_err(), "{header} was allowed");
        }
        assert!(rules::check_header("content-type").is_ok());
    }

    #[test]
    fn a_credential_belongs_to_the_rule_that_matched() {
        // Not to the request, and not to the host the caller named: swapping
        // one host's credential onto another is the leak this guards.
        let paid = with_credential("api.stripe.com", "authorization", "STRIPE_KEY");
        let free = rule("docs.example.com");

        assert_eq!(credential_for(&paid), Some(("authorization", "STRIPE_KEY")));
        assert_eq!(credential_for(&free), None);
    }
}

#[cfg(test)]
mod gating {
    use crate::egress::gate::{Gate, Gates};

    fn gate() -> Gate {
        Gate {
            requires: "charge".into(),
            host: "outturn-hollowbrook".into(),
            method: "POST".into(),
            path: "/charges".into(),
            identified_by: Some("booking_id".into()),
            binds: vec!["amount_pence".into()],
        }
    }

    /// A request covered by two gates goes out only once both are approved.
    ///
    /// The bypass this replaces: the gateway asked only the first covering
    /// gate, and `/*` sorts before `/refunds`. So with `approve_new_hosts` on, a
    /// person approving "reach this host" for a `POST /refunds` minted a grant
    /// for the host's gate, and the refund went out with its own gate never
    /// consulted.
    #[test]
    fn every_gate_covering_a_request_must_be_approved() {
        use crate::egress::grant::{Extent, Granted, digest};

        let reach = Gate {
            requires: "reach".into(),
            host: "pay.example.com".into(),
            method: "POST".into(),
            path: "/*".into(),
            identified_by: None,
            binds: vec![],
        };
        let refund = Gate {
            requires: "refund".into(),
            host: "pay.example.com".into(),
            method: "POST".into(),
            path: "/refunds".into(),
            identified_by: None,
            binds: vec!["amount_pence".into()],
        };
        let body = Some(r#"{"amount_pence":90000}"#);
        let grant = |gate: &Gate| Granted {
            requires: gate.requires.clone(),
            extent: Extent::Call,
            keyed_on: digest(gate, "POST", "pay.example.com", "/refunds", body),
        };

        let reach_only =
            Gates::of(vec![reach.clone(), refund.clone()]).with_grants(vec![grant(&reach)]);
        assert_eq!(
            reach_only
                .unpermitted("pay.example.com", "POST", "/refunds", body)
                .map(|g| g.requires.as_str()),
            Some("refund"),
            "approving the host must not approve the refund"
        );

        let both = Gates::of(vec![reach.clone(), refund.clone()])
            .with_grants(vec![grant(&reach), grant(&refund)]);
        assert!(
            both.unpermitted("pay.example.com", "POST", "/refunds", body)
                .is_none(),
            "with both approved it goes out"
        );
    }

    /// The gate set offered has to be the one the API committed to.
    ///
    /// This is the check that makes the whole scheme worth anything: a runtime
    /// that dropped a gate from its copy would otherwise get the request through,
    /// which is precisely what a compromised one would do.
    #[test]
    fn a_set_that_does_not_hash_to_the_commitment_is_refused() {
        let workspace = uuid::Uuid::from_bytes([3; 16]);
        let real = Gates::of(vec![gate()]);
        let stripped = Gates::none();

        let committed = real.root(workspace);
        assert!(
            !stripped.matches(workspace, &committed),
            "a stripped set passed the commitment"
        );
        assert!(real.matches(workspace, &committed));
    }

    /// And an empty set is a statement, not an absence.
    ///
    /// A turn nothing gates says so, signed. What is refused is a token carrying
    /// no gate claim at all, because defaulting that to empty would mean nothing
    /// is ever gated -- the answer a forger would choose.
    #[test]
    fn a_turn_gated_by_nothing_still_commits_to_that() {
        let workspace = uuid::Uuid::from_bytes([3; 16]);
        let none = Gates::none();
        assert!(none.matches(workspace, &none.root(workspace)));
        assert!(!Gates::of(vec![gate()]).matches(workspace, &none.root(workspace)));
    }

    /// A gated request is found by host, method and path together.
    #[test]
    fn only_the_gated_request_is_covered() {
        let gates = Gates::of(vec![gate()]);
        assert!(
            gates
                .covering("outturn-hollowbrook", "POST", "/charges")
                .is_some()
        );
        // The same path on another host, the same host with another method, and
        // the same method on another path all go through.
        assert!(gates.covering("example.com", "POST", "/charges").is_none());
        assert!(
            gates
                .covering("outturn-hollowbrook", "GET", "/charges")
                .is_none()
        );
        assert!(
            gates
                .covering("outturn-hollowbrook", "POST", "/bookings")
                .is_none()
        );
    }

    /// What an approval is worth, at the tier that decides whether a request
    /// goes out.
    ///
    /// The end of the loop and the part no end-to-end run has yet reached: a
    /// resumed turn re-derives what it was doing through a model, and in every
    /// live attempt so far it found the room already booked by its own earlier
    /// try and declined before charging anything. That is honest model
    /// behaviour, and it means the gateway honouring a grant was never
    /// exercised -- so it is exercised here, where it is decidable.
    #[test]
    fn a_granted_request_is_let_through_the_gate_that_refused_it() {
        use crate::egress::grant::{Extent, Granted, digest};

        let gate = gate();
        let body = r#"{"booking_id":"bk_8812","amount_pence":12000}"#;

        // Refused first: without a grant the gate still catches it.
        let bare = Gates::of(vec![gate.clone()]);
        assert!(
            bare.covering("outturn-hollowbrook", "POST", "/charges")
                .is_some(),
            "the gate has to catch this request or the test proves nothing"
        );
        assert!(
            bare.permitted(&gate, "POST", "outturn-hollowbrook", "/charges", Some(body))
                .is_none(),
            "nothing is permitted before anybody approves"
        );

        // And let through once the turn carries the grant that answer minted.
        let granted = Gates::of(vec![gate.clone()]).with_grants(vec![Granted {
            requires: "charge".into(),
            extent: Extent::Call,
            keyed_on: digest(&gate, "POST", "outturn-hollowbrook", "/charges", Some(body)),
        }]);
        assert!(
            granted
                .permitted(&gate, "POST", "outturn-hollowbrook", "/charges", Some(body))
                .is_some(),
            "an approved request must go out, or the yes bought nothing"
        );

        // But only that request. The same grant against a larger amount is the
        // £40-approval-covers-£4,000 hole the digest exists to close.
        let larger = r#"{"booking_id":"bk_8812","amount_pence":400000}"#;
        assert!(
            granted
                .permitted(
                    &gate,
                    "POST",
                    "outturn-hollowbrook",
                    "/charges",
                    Some(larger)
                )
                .is_none(),
            "a grant must not cover a charge nobody was shown"
        );
    }

    /// A grant the runtime added to its copy does not verify, so it cannot be
    /// used to let anything through.
    #[test]
    fn a_grant_that_is_not_committed_to_is_not_offered() {
        use crate::egress::grant::{Extent, Granted};

        let workspace = uuid::Uuid::from_bytes([7; 16]);
        let committed = Gates::of(vec![gate()]);
        let root = committed.root(workspace);

        let forged = Gates::of(vec![gate()]).with_grants(vec![Granted {
            requires: "charge".into(),
            extent: Extent::Call,
            keyed_on: "whatever this runtime wants".into(),
        }]);

        assert!(
            !forged.matches(workspace, &root),
            "a grant nobody committed to must fail the commitment check, which is \
             what stops it ever reaching the permit check"
        );
    }
}

#[cfg(test)]
mod client_credentials {
    use super::*;

    use crate::runtime::egress::EgressRule;

    /// The whole path, as a turn sees it: a token exchanged and attached as a
    /// bearer, held for the next request, and forgotten when the host says it
    /// no longer accepts it -- with the 401 passed back rather than retried.
    #[tokio::test]
    async fn a_client_credentials_rule_attaches_its_token_and_drops_it_on_a_401() {
        use std::sync::Mutex;
        use transport::{TransportResponse, VettedRequest};

        /// The token endpoint at `/token`, and an API answering from a script.
        struct World {
            exchanges: Mutex<u32>,
            api: Mutex<Vec<u16>>,
            seen: Mutex<Vec<String>>,
        }

        #[async_trait::async_trait]
        impl transport::EgressTransport for World {
            fn name(&self) -> &'static str {
                "test"
            }

            async fn send(
                &self,
                request: VettedRequest,
            ) -> Result<TransportResponse, TransportError> {
                let (status, body) = if request.url.path() == "/token" {
                    let mut n = self.exchanges.lock().unwrap();
                    *n += 1;
                    (
                        200,
                        format!(
                            r#"{{"access_token":"tok-{n}","token_type":"Bearer","expires_in":3600}}"#
                        ),
                    )
                } else {
                    let auth = request
                        .headers
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_string())
                        .unwrap_or_default();
                    self.seen.lock().unwrap().push(auth);
                    (self.api.lock().unwrap().remove(0), "{}".to_string())
                };
                Ok(TransportResponse {
                    status,
                    headers: Vec::new(),
                    body,
                    truncated: false,
                })
            }
        }

        // SAFETY: names no other test uses.
        unsafe {
            std::env::set_var("OUTTURN_EGRESS_TEST_FETCH_CC_ID", "id");
            std::env::set_var("OUTTURN_EGRESS_TEST_FETCH_CC_SECRET", "secret");
        }
        // Literal public addresses, so nothing is asked of a resolver.
        let rule = EgressRule {
            host: "93.184.215.14".into(),
            header: None,
            credential_env: None,
            client: Some(crate::runtime::egress::ClientCredentials {
                token_url: "https://93.184.215.15/token".into(),
                scope: None,
                client_id_env: "OUTTURN_EGRESS_TEST_FETCH_CC_ID".into(),
                client_secret_env: "OUTTURN_EGRESS_TEST_FETCH_CC_SECRET".into(),
                client_auth: crate::runtime::egress::ClientAuth::Basic,
            }),
            credential: None,
        };

        let seed = [9u8; 32];
        let minter = crate::auth::TokenMinter::new(&seed).expect("minter");
        let validator = crate::auth::TokenValidator::new(
            &crate::auth::TokenMinter::public_key_of(&seed),
            crate::auth::AUDIENCE_GATEWAY,
        )
        .expect("validator");
        let world = Arc::new(World {
            exchanges: Mutex::new(0),
            api: Mutex::new(vec![200, 401, 200]),
            seen: Mutex::new(Vec::new()),
        });
        let state = Arc::new(
            super::super::GatewayState::new(Vec::new(), validator)
                .with_egress_transport(world.clone())
                .with_bindings(
                    crate::egress::bindings::Bindings::parse(
                        r#"{
                            "OUTTURN_EGRESS_TEST_FETCH_CC_ID": {"workspaces": ["*"],
                                "hosts": ["93.184.215.14"], "token_url": "https://93.184.215.15/token"},
                            "OUTTURN_EGRESS_TEST_FETCH_CC_SECRET": {"workspaces": ["*"],
                                "hosts": ["93.184.215.14"], "token_url": "https://93.184.215.15/token"}
                        }"#,
                    )
                    .expect("readable"),
                ),
        );

        let workspace = uuid::Uuid::now_v7();
        let rules = vec![rule.clone()];
        let token = minter
            .mint_turn(
                uuid::Uuid::now_v7(),
                workspace,
                commit::root(workspace, &rules),
                crate::egress::gate::Gates::none().root(workspace),
            )
            .expect("turn token");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());

        let mut statuses = Vec::new();
        for _ in 0..3 {
            let request = EgressRequest {
                method: "GET".into(),
                url: "https://93.184.215.14/bookings".into(),
                gates: crate::egress::gate::Gates::none(),
                headers: Vec::new(),
                body: None,
                proof: commit::prove(workspace, &rules, &rule).expect("in the set"),
            };
            let got = fetch(State(state.clone()), headers.clone(), Json(request))
                .await
                .map_err(|(s, m)| format!("{s}: {m}"))
                .expect("allowed");
            statuses.push(got.0.status);
        }

        assert_eq!(statuses, vec![200, 401, 200], "the 401 went back as it was");
        assert_eq!(
            *world.seen.lock().unwrap(),
            vec!["Bearer tok-1", "Bearer tok-1", "Bearer tok-2"],
            "held for the second request, exchanged afresh after the 401"
        );
        assert_eq!(*world.exchanges.lock().unwrap(), 2);
    }

    /// The leak bindings close: a workspace writing a rule that names a
    /// variable the operator set up for another, on a host it controls. Its
    /// own rule, honestly committed -- and still refused, before anything is
    /// read or sent.
    #[tokio::test]
    async fn a_workspace_cannot_send_another_workspaces_credential() {
        use std::sync::Mutex;
        use transport::{TransportResponse, VettedRequest};

        struct Recorder(Mutex<Vec<String>>);

        #[async_trait::async_trait]
        impl transport::EgressTransport for Recorder {
            fn name(&self) -> &'static str {
                "test"
            }

            async fn send(
                &self,
                request: VettedRequest,
            ) -> Result<TransportResponse, TransportError> {
                self.0.lock().unwrap().push(request.url.to_string());
                Ok(TransportResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: "{}".into(),
                    truncated: false,
                })
            }
        }

        // SAFETY: names no other test uses.
        unsafe {
            std::env::set_var("OUTTURN_EGRESS_TEST_ACME_STRIPE", "sk_live_acme");
            std::env::set_var("OUTTURN_EGRESS_TEST_ACME_ID", "acme-id");
            std::env::set_var("OUTTURN_EGRESS_TEST_ACME_SECRET", "acme-secret");
        }
        let acme = uuid::Uuid::now_v7();
        let thief = uuid::Uuid::now_v7();
        // Literal public addresses, so nothing is asked of a resolver: .14 is
        // ACME's API, .15 its token endpoint, .66 the thief's.
        let bindings = crate::egress::bindings::Bindings::parse(&format!(
            r#"{{
                "OUTTURN_EGRESS_TEST_ACME_STRIPE": {{"workspaces": ["{acme}"], "hosts": ["93.184.215.14"]}},
                "OUTTURN_EGRESS_TEST_ACME_ID": {{"workspaces": ["{acme}"], "hosts": ["93.184.215.14"],
                    "token_url": "https://93.184.215.15/token"}},
                "OUTTURN_EGRESS_TEST_ACME_SECRET": {{"workspaces": ["{acme}"], "hosts": ["93.184.215.14"],
                    "token_url": "https://93.184.215.15/token"}}
            }}"#
        ))
        .expect("readable");

        let seed = [11u8; 32];
        let minter = crate::auth::TokenMinter::new(&seed).expect("minter");
        let validator = crate::auth::TokenValidator::new(
            &crate::auth::TokenMinter::public_key_of(&seed),
            crate::auth::AUDIENCE_GATEWAY,
        )
        .expect("validator");
        let sent = Arc::new(Recorder(Mutex::new(Vec::new())));
        let state = Arc::new(
            super::super::GatewayState::new(Vec::new(), validator)
                .with_egress_transport(sent.clone())
                .with_bindings(bindings),
        );

        let ask = |workspace: uuid::Uuid, rule: EgressRule, url: &str| {
            let state = state.clone();
            let rules = vec![rule.clone()];
            let token = minter
                .mint_turn(
                    uuid::Uuid::now_v7(),
                    workspace,
                    commit::root(workspace, &rules),
                    crate::egress::gate::Gates::none().root(workspace),
                )
                .expect("turn token");
            let mut headers = HeaderMap::new();
            headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
            let request = EgressRequest {
                method: "GET".into(),
                url: url.into(),
                gates: crate::egress::gate::Gates::none(),
                headers: Vec::new(),
                body: None,
                proof: commit::prove(workspace, &rules, &rule).expect("in the set"),
            };
            async move {
                fetch(State(state), headers, Json(request))
                    .await
                    .map(|_| ())
            }
        };
        let header = |host: &str| EgressRule {
            host: host.into(),
            header: Some("authorization".into()),
            credential_env: Some("OUTTURN_EGRESS_TEST_ACME_STRIPE".into()),
            client: None,
            credential: None,
        };
        let client = |host: &str, token_url: &str| EgressRule {
            host: host.into(),
            header: None,
            credential_env: None,
            client: Some(crate::runtime::egress::ClientCredentials {
                token_url: token_url.into(),
                scope: None,
                client_id_env: "OUTTURN_EGRESS_TEST_ACME_ID".into(),
                client_secret_env: "OUTTURN_EGRESS_TEST_ACME_SECRET".into(),
                client_auth: crate::runtime::egress::ClientAuth::Basic,
            }),
            credential: None,
        };

        // The thief's own host, ACME's variable.
        let (status, why) = ask(
            thief,
            header("93.184.215.66"),
            "https://93.184.215.66/collect",
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(why.contains("not bound to this workspace"), "{why}");

        // Even at ACME's own host: the variable is ACME's to use.
        let (_, why) = ask(
            thief,
            header("93.184.215.14"),
            "https://93.184.215.14/charges",
        )
        .await
        .unwrap_err();
        assert!(why.contains("not bound to this workspace"), "{why}");

        // The client secret, at a token endpoint the thief names.
        let (status, why) = ask(
            thief,
            client("93.184.215.66", "https://93.184.215.66/token"),
            "https://93.184.215.66/collect",
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(why.contains("not bound to this workspace"), "{why}");

        // And ACME itself cannot aim its own secret somewhere else.
        let (_, why) = ask(
            acme,
            client("93.184.215.14", "https://93.184.215.66/token"),
            "https://93.184.215.14/ledger",
        )
        .await
        .unwrap_err();
        assert!(why.contains("token endpoint"), "{why}");

        assert!(
            sent.0.lock().unwrap().is_empty(),
            "nothing left the gateway: {:?}",
            sent.0.lock().unwrap()
        );

        // Where it is bound, it goes.
        ask(
            acme,
            header("93.184.215.14"),
            "https://93.184.215.14/charges",
        )
        .await
        .expect("bound to ACME at its host");
    }
}
