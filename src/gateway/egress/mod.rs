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

pub mod internal;
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
    let claims = super::router::authenticate(&state, &headers)?;

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
    if let Some(gate) = request.gates.covering(&host, &method, url.path()) {
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

    // A credential travels only where it cannot be read on the way. A
    // workspace that configured a key for a host did not consent to it going
    // out in clear because a model typed http.
    if rule.credential_env.is_some() && url.scheme() != "https" {
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
        body_limit: FETCH_BODY_LIMIT,
    };

    match state.egress_transport.send(vetted).await {
        Ok(response) => Ok(Json(EgressResponse {
            status: response.status,
            headers: response.headers,
            body: response.body,
            truncated: response.truncated,
        })),
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
        }
    }

    fn with_credential(host: &str, header: &str, env: &str) -> EgressRule {
        EgressRule {
            host: host.into(),
            header: Some(header.into()),
            credential_env: Some(env.into()),
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
        }
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
}
