//! What an agent is allowed to reach, and how that is decided.
//!
//! Two questions, asked in this order and both by the host.
//!
//! Is the workspace willing? An agent reaches nothing by default. A workspace names
//! the hosts its agents may call, and anything unnamed is refused -- a default
//! of "allow" would mean a prompt injection is a data exfiltration primitive,
//! and the workspace would have consented to it by not thinking about it.
//!
//! Is the address safe? A name a workspace allowed can still resolve somewhere
//! nobody meant: the cluster's own gateway, the database, the node's metadata
//! service. That check is not the workspace's to waive, so it is applied after
//! theirs and cannot be configured away.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// One host a workspace's agents may reach.
///
/// Deliberately about hosts rather than URLs. A workspace adding an API knows its
/// hostname and would have to guess at its paths, and a rule written in paths
/// silently stops matching when the vendor reorganises them. Scheme, port and
/// path are the request's business; whether this host may be spoken to at all
/// is the rule's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EgressRule {
    /// `api.stripe.com`, or `*.example.com` for its subdomains.
    pub host: String,

    /// A header the host attaches on the way out, if any.
    ///
    /// The guest never sees it and cannot set it: a credential a component can
    /// read is a credential a component can send somewhere else.
    #[serde(default)]
    pub header: Option<String>,
    /// Name of the environment variable holding that header's value.
    ///
    /// A name rather than the secret, so credentials live where the platform
    /// already keeps secrets and never sit in a row that a backup, a log line
    /// or a support query could carry off.
    #[serde(default)]
    pub credential_env: Option<String>,
}

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Not http or https.
    Scheme(String),
    /// The URL carries no host, or one that cannot be parsed.
    NoHost,
    /// The workspace has not allowed this host.
    NotAllowed(String),
    /// Allowed by name, but the name resolves somewhere it must not reach.
    PrivateAddress { host: String, addr: IpAddr },
    /// The name resolves to nothing at all.
    Unresolvable(String),
    /// A header the host owns, or one that has no business crossing.
    Header(String),
    /// The rule matched by `host` could not be shown to be one the API
    /// committed to for this turn -- either it is not in the set the
    /// commitment was built over, or the proof offered for it did not verify.
    /// The runtime holds the rules but does not decide what a workspace
    /// allowed, so this is what "I cannot tell" has to mean: refused, the
    /// same as a host nobody named.
    Unproven(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Worded for whoever reads it in a transcript. A model that is told
            // only "refused" tries again the same way.
            Refused::Scheme(s) => write!(f, "{s} is not a scheme this can speak; use https"),
            Refused::NoHost => write!(f, "that URL names no host"),
            Refused::NotAllowed(h) => write!(
                f,
                "{h} is not on this workspace's allowed list; someone with access to \
                 settings can add it"
            ),
            Refused::PrivateAddress { host, addr } => write!(
                f,
                "{host} resolves to {addr}, which is inside the network running this \
                 service and cannot be reached from an agent"
            ),
            Refused::Unresolvable(h) => write!(f, "{h} does not resolve"),
            Refused::Header(h) => write!(f, "the {h} header is set by the platform, not by you"),
            Refused::Unproven(h) => write!(
                f,
                "{h} could not be shown to be one of this turn's allowed hosts; if it was \
                 just added to settings, this turn was started before that took effect"
            ),
        }
    }
}

/// Headers a guest may not set.
///
/// `authorization` and the vendor key headers because the host attaches those
/// and a guest that could overwrite one could use a workspace's credential
/// against a different endpoint. `host` because it decides which site a
/// request reaches, independently of the URL that was checked.
const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "host",
    "x-api-key",
];

/// Whether a host matches a rule.
///
/// `*.example.com` covers any subdomain, at any depth, and deliberately not
/// `example.com` itself: a workspace allowing subdomains has said nothing about
/// the apex, and the apex is usually where the interesting things are.
pub fn host_matches(rule: &str, host: &str) -> bool {
    let rule = rule.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();

    if rule.is_empty() || host.is_empty() {
        return false;
    }

    match rule.strip_prefix("*.") {
        // The dot is part of the suffix, so `*.example.com` cannot be
        // satisfied by `notexample.com`.
        Some(suffix) => !suffix.is_empty() && host.ends_with(&format!(".{suffix}")),
        None => rule == host,
    }
}

/// Whether an operator has opened this bare name.
///
/// A name with no dot can only be something in the network running this
/// service, and naming it in `OUTTURN_INTERNAL_HOSTS` is exactly how an
/// operator says that is intended. Refusing it regardless made the two halves
/// of one decision disagree: the gateway would connect to `tickets`, and no
/// rule permitting `tickets` could be written for it to match -- so the
/// allowlist opened a path nothing could use.
///
/// Consulted rather than waived. A workspace still cannot invent `outturn-api`
/// as a rule; it can only name what somebody outside it already opened, which
/// is the distinction docs/egress.md draws and the reason the list is
/// configuration rather than a table.
fn opened_by_operator(host: &str) -> bool {
    crate::gateway::egress::internal::Internal::shared().names(host)
}

/// Turns what someone typed into a rule, or explains why it is not one.
///
/// Deliberately forgiving about form and strict about meaning. Someone adding
/// an API has its documentation open and will paste what is in front of them:
/// `https://api.stripe.com/v1/charges`, or `API.Stripe.com`, or a trailing
/// slash. All of those name the same host and all of them are accepted. What
/// is refused is a rule that would not mean what its author thought.
pub fn normalise_host(input: &str) -> Result<String, String> {
    let mut host = input.trim().to_ascii_lowercase();

    // Pasted from a browser or a curl example.
    if let Some((_, rest)) = host.split_once("://") {
        host = rest.to_string();
    }
    // A path, a query, or credentials in front of the host.
    if let Some((before, _)) = host.split_once('/') {
        host = before.to_string();
    }
    if let Some((_, after)) = host.rsplit_once('@') {
        host = after.to_string();
    }
    // A port says which door, not which building, and a rule is about the
    // building.
    if let Some((before, after)) = host.rsplit_once(':')
        && after.chars().all(|c| c.is_ascii_digit())
    {
        host = before.to_string();
    }
    let host = host.trim_end_matches('.').to_string();

    if host.is_empty() {
        return Err("that is not a hostname".into());
    }

    // A bare wildcard is almost always a misunderstanding of what the list is
    // for, and the one case where being strict is kinder than being helpful.
    if host == "*" || host == "*." {
        return Err(
            "a rule has to name a host. Allowing everything would mean anything an agent              is persuaded to read could be sent anywhere"
                .into(),
        );
    }

    let labels = host.strip_prefix("*.").unwrap_or(&host);
    if labels.contains('*') {
        return Err("a wildcard can only stand for the leftmost part, as in *.example.com".into());
    }

    if labels.parse::<IpAddr>().is_ok() {
        // Allowed as a rule, because a public address is a legitimate thing to
        // name -- but a private one never becomes reachable, and saying so now
        // is better than at three in the morning inside an agent's transcript.
        if let Ok(addr) = labels.parse::<IpAddr>()
            && is_forbidden(addr)
        {
            return Err(format!(
                "{addr} is inside the network running this service, so a rule for it                  would never permit anything"
            ));
        }
        return Ok(host);
    }

    if !labels.contains('.') && !opened_by_operator(labels) {
        return Err(format!(
            "{labels} has no domain, so it can only name something inside the network              running this service. An operator opens one by naming it in              OUTTURN_INTERNAL_HOSTS."
        ));
    }

    let valid = labels.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    });
    if !valid {
        return Err(format!("{host} is not a hostname"));
    }

    Ok(host)
}

/// The rule permitting this host, if a workspace wrote one.
pub fn rule_for<'a>(rules: &'a [EgressRule], host: &str) -> Option<&'a EgressRule> {
    // First match wins, and exact rules are tried before wildcards so a
    // specific host can carry its own credential while its siblings share
    // another.
    rules
        .iter()
        .find(|r| !r.host.starts_with("*.") && host_matches(&r.host, host))
        .or_else(|| rules.iter().find(|r| host_matches(&r.host, host)))
}

/// Whether an address is one an agent must never be pointed at.
///
/// Not a list of the cluster's own addresses, which would need maintaining and
/// would be wrong the first time something moved. Everything that is not
/// plainly on the public internet is refused instead, so a new internal
/// service is covered by having been born.
pub fn is_forbidden(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => {
            // An address that is really IPv4 wearing a v6 coat must be judged
            // as what it is, or ::ffff:127.0.0.1 walks straight past.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_v4(v4);
            }
            is_forbidden_v6(v6)
        }
    }
}

fn is_forbidden_v4(addr: Ipv4Addr) -> bool {
    let [a, b, ..] = addr.octets();
    addr.is_loopback()
        || addr.is_private()
        || addr.is_link_local()
        || addr.is_broadcast()
        || addr.is_multicast()
        || addr.is_documentation()
        || addr.is_unspecified()
        // Carrier-grade NAT, which a cluster may sit behind.
        || (a == 100 && (64..128).contains(&b))
        // Reserved, and 0.0.0.0/8, which some stacks route to localhost.
        || a == 0
        || a >= 240
}

fn is_forbidden_v6(addr: Ipv6Addr) -> bool {
    let first = addr.segments()[0];
    addr.is_loopback()
        || addr.is_multicast()
        || addr.is_unspecified()
        // Unique local, fc00::/7 -- the v6 equivalent of a private range.
        || (first & 0xfe00) == 0xfc00
        // Link local, fe80::/10, which carries the metadata services.
        || (first & 0xffc0) == 0xfe80
}

/// Checks a header a guest wants to set.
pub fn check_header(name: &str) -> Result<(), Refused> {
    let lowered = name.trim().to_ascii_lowercase();
    if RESERVED_HEADERS.contains(&lowered.as_str()) {
        return Err(Refused::Header(lowered));
    }
    Ok(())
}

/// Checks a URL against a workspace's rules, before anything is resolved.
///
/// Returns the host and the rule that admitted it, so the caller can resolve
/// once and attach whatever credential the rule names.
pub fn check_url<'a>(
    rules: &'a [EgressRule],
    url: &reqwest::Url,
) -> Result<(String, &'a EgressRule), Refused> {
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(Refused::Scheme(other.to_string())),
    }

    let host = url.host_str().ok_or(Refused::NoHost)?.to_string();

    // A literal address in the URL never matches a name rule, so this is
    // already refused above -- but say so precisely, because "not allowed" and
    // "not allowed to reach the cluster" are different problems.
    if let Ok(addr) = host.trim_matches(['[', ']']).parse::<IpAddr>()
        && is_forbidden(addr)
    {
        return Err(Refused::PrivateAddress { host, addr });
    }

    let rule = rule_for(rules, &host).ok_or_else(|| Refused::NotAllowed(host.clone()))?;
    Ok((host, rule))
}

/// Vets what a name resolved to.
///
/// Every address, not merely the first. A name under someone else's control
/// can answer with a public address and a private one together, and a check
/// that stops at the first acceptable answer lets the connection be made to
/// the other.
pub fn vet_addresses(host: &str, addrs: &[std::net::SocketAddr]) -> Result<(), Refused> {
    if addrs.is_empty() {
        return Err(Refused::Unresolvable(host.to_string()));
    }
    for addr in addrs {
        if is_forbidden(addr.ip()) {
            return Err(Refused::PrivateAddress {
                host: host.to_string(),
                addr: addr.ip(),
            });
        }
    }
    Ok(())
}

/// Shows that a matched rule is one the API committed to for this turn.
///
/// The runtime holds the rule list because it travels with the turn, not
/// because the runtime is trusted to say what a workspace allowed -- that
/// decision belongs to the tier that never runs workspace code. So a rule is
/// not used on the strength of being found in `rules`; it has to be provable
/// against the commitment the turn arrived with. `commit::prove` failing and
/// `commit::verify` failing are both the same fact from here: this rule
/// cannot be shown to be genuine, so the request does not go out. There is no
/// third path that lets a fetch proceed without a proof that verified.
pub fn vet_commitment(
    workspace_id: uuid::Uuid,
    rules: &[EgressRule],
    commitment: &crate::egress::commit::Hash,
    rule: &EgressRule,
    host: &str,
) -> Result<EgressRule, Refused> {
    let proof = crate::egress::commit::prove(workspace_id, rules, rule)
        .ok_or_else(|| Refused::Unproven(host.to_string()))?;
    let vouched = crate::egress::commit::verify(workspace_id, commitment, &proof)
        .map_err(|_| Refused::Unproven(host.to_string()))?;

    // The rule that was vouched for, handed back rather than dropped. `verify`
    // returns rules precisely so that checking one and then using another is
    // not a thing a caller can do by accident, and answering `()` here would
    // put that mistake back within reach of the next person to edit `fetch`.
    // For a whole-set proof the vouched list is every rule, so the one to use
    // is still the one that matched.
    vouched
        .into_iter()
        .find(|r| r == rule)
        .ok_or_else(|| Refused::Unproven(host.to_string()))
}

/// Resolves a host and vets the result, returning what may be connected to.
///
/// The addresses come back so the caller can connect to exactly these. Between
/// a check and a connection the name can be made to answer differently -- the
/// check passes on a public address and the connection lands on a private one,
/// which is the whole of DNS rebinding. Resolving once and pinning the answer
/// closes that, because the name is never asked twice.
pub async fn resolve_and_vet(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, Refused> {
    let addrs = resolve(host, port).await?;
    vet_addresses(host, &addrs)?;
    Ok(addrs)
}

/// Where a host is, without judging whether it may be reached.
///
/// Split out for the one caller that has already been told the answer: a host
/// an operator opened is reached despite being private, and the judgement is
/// the only part being skipped. Resolution still happens and the addresses are
/// still pinned, so what was checked and what is connected to stay the same
/// place -- which is the property that would be lost by resolving twice.
pub async fn resolve(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, Refused> {
    // A literal address needs no lookup, and asking for one would let a
    // resolver answer for it.
    if let Ok(addr) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(vec![std::net::SocketAddr::new(addr, port)]);
    }

    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| Refused::Unresolvable(host.to_string()))?
        .collect();

    if addrs.is_empty() {
        return Err(Refused::Unresolvable(host.to_string()));
    }
    Ok(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_exact_host_matches_only_itself() {
        assert!(host_matches("api.stripe.com", "api.stripe.com"));
        assert!(!host_matches("api.stripe.com", "evil.com"));
        assert!(!host_matches("api.stripe.com", "sub.api.stripe.com"));
    }

    #[test]
    fn matching_ignores_case_and_a_trailing_dot() {
        // Both are the same name, and a check that disagrees is bypassable.
        assert!(host_matches("API.Stripe.com", "api.stripe.com"));
        assert!(host_matches("api.stripe.com", "api.stripe.com."));
    }

    #[test]
    fn a_wildcard_covers_subdomains_but_not_the_apex() {
        assert!(host_matches("*.example.com", "api.example.com"));
        assert!(host_matches("*.example.com", "a.b.example.com"));
        assert!(
            !host_matches("*.example.com", "example.com"),
            "allowing subdomains said nothing about the apex"
        );
    }

    #[test]
    fn a_wildcard_cannot_be_satisfied_by_a_lookalike() {
        // The classic: the suffix must begin at a label boundary.
        assert!(!host_matches("*.example.com", "notexample.com"));
        assert!(!host_matches("*.example.com", "example.com.evil.net"));
    }

    #[test]
    fn nothing_matches_an_empty_rule() {
        assert!(!host_matches("", "example.com"));
        assert!(!host_matches("*.", "example.com"));
    }

    #[test]
    fn an_exact_rule_is_preferred_over_a_wildcard() {
        // So one host can carry its own credential while its siblings share
        // another.
        let rules = vec![
            rule("*.example.com"),
            EgressRule {
                host: "api.example.com".into(),
                header: Some("authorization".into()),
                credential_env: Some("EXAMPLE_KEY".into()),
            },
        ];
        let found = rule_for(&rules, "api.example.com").expect("matched");
        assert_eq!(found.credential_env.as_deref(), Some("EXAMPLE_KEY"));
        assert!(
            rule_for(&rules, "other.example.com")
                .expect("matched")
                .credential_env
                .is_none()
        );
    }

    #[test]
    fn what_someone_pastes_becomes_a_rule() {
        // Whatever is in front of them when they go looking for the hostname.
        for typed in [
            "https://api.stripe.com/v1/charges",
            "API.Stripe.com",
            "api.stripe.com/",
            "https://api.stripe.com:443",
            "  api.stripe.com  ",
            "api.stripe.com.",
        ] {
            assert_eq!(
                normalise_host(typed).expect(typed),
                "api.stripe.com",
                "{typed:?} did not become a rule"
            );
        }
        assert_eq!(
            normalise_host("*.EXAMPLE.com").expect("wildcard"),
            "*.example.com"
        );
    }

    #[test]
    fn a_rule_that_would_not_mean_what_it_says_is_refused() {
        for typed in [
            // Allowing everything, which is never what someone means to click.
            "*",
            // A wildcard anywhere but the front does not mean what it looks
            // like it means.
            "api.*.com",
            "",
            "   ",
            // No domain: only something inside this network answers to it.
            "localhost",
            "postgres",
            "-bad.example.com",
        ] {
            assert!(normalise_host(typed).is_err(), "{typed:?} was accepted");
        }
    }

    #[test]
    fn an_address_inside_the_cluster_is_refused_when_it_is_typed() {
        // It would never permit anything, and finding that out here is better
        // than finding it out in an agent's transcript.
        let refused = normalise_host("10.0.0.5").expect_err("accepted");
        assert!(refused.contains("inside the network"), "{refused}");
        assert!(normalise_host("169.254.169.254").is_err());
        // A public address is a legitimate thing to name.
        assert_eq!(normalise_host("1.1.1.1").expect("public"), "1.1.1.1");
    }

    #[test]
    fn nothing_is_reachable_by_default() {
        let url = reqwest::Url::parse("https://api.stripe.com/v1/charges").expect("url");
        assert_eq!(
            check_url(&[], &url).expect_err("an empty list allowed a request"),
            Refused::NotAllowed("api.stripe.com".into())
        );
    }

    #[test]
    fn only_http_and_https_are_spoken() {
        for url in ["file:///etc/passwd", "ftp://example.com/x", "gopher://a/b"] {
            let url = reqwest::Url::parse(url).expect("url");
            assert!(
                matches!(check_url(&[rule("*.com")], &url), Err(Refused::Scheme(_))),
                "{url} was not refused"
            );
        }
    }

    #[test]
    fn the_cluster_is_not_reachable_by_naming_its_address() {
        for host in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            // The metadata service, which is the whole reason this exists.
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
        ] {
            let url = reqwest::Url::parse(&format!("http://{host}/")).expect("url");
            let rules = vec![rule(host)];
            assert!(
                matches!(check_url(&rules, &url), Err(Refused::PrivateAddress { .. })),
                "{host} was reachable even though a rule named it"
            );
        }
    }

    #[test]
    fn an_ipv4_address_in_a_v6_coat_is_judged_as_ipv4() {
        // ::ffff:127.0.0.1 is loopback, and a check that looks only at the v6
        // rules waves it through.
        assert!(is_forbidden("::ffff:127.0.0.1".parse().expect("addr")));
        assert!(is_forbidden(
            "::ffff:169.254.169.254".parse().expect("addr")
        ));
    }

    #[test]
    fn the_v6_private_ranges_are_refused() {
        for addr in ["::1", "fc00::1", "fd12:3456::1", "fe80::1", "::"] {
            assert!(
                is_forbidden(addr.parse().expect("addr")),
                "{addr} was allowed"
            );
        }
    }

    #[test]
    fn one_bad_answer_among_good_ones_refuses_the_lot() {
        // A name someone else controls can answer with both. Stopping at the
        // first acceptable address is how the connection ends up on the other.
        let addrs = vec![
            "93.184.216.34:443".parse().expect("addr"),
            "127.0.0.1:443".parse().expect("addr"),
        ];
        assert!(matches!(
            vet_addresses("mixed.example.com", &addrs),
            Err(Refused::PrivateAddress { .. })
        ));
    }

    #[test]
    fn a_name_that_resolves_to_nothing_is_refused_as_such() {
        assert_eq!(
            vet_addresses("nowhere.example", &[]).expect_err("empty was accepted"),
            Refused::Unresolvable("nowhere.example".into())
        );
    }

    #[tokio::test]
    async fn a_literal_address_is_never_looked_up() {
        // Asking a resolver about an address it was given is a way for the
        // resolver to answer with a different one.
        let addrs = resolve_and_vet("93.184.216.34", 443).await.expect("vetted");
        assert_eq!(addrs, vec!["93.184.216.34:443".parse().expect("addr")]);
        assert!(resolve_and_vet("127.0.0.1", 443).await.is_err());
    }

    #[test]
    fn ordinary_public_addresses_are_allowed() {
        for addr in ["1.1.1.1", "93.184.216.34", "2606:4700::1111"] {
            assert!(
                !is_forbidden(addr.parse().expect("addr")),
                "{addr} was refused"
            );
        }
    }

    #[test]
    fn the_platforms_own_headers_cannot_be_set_by_a_guest() {
        for header in [
            "Authorization",
            "authorization",
            "COOKIE",
            "Host",
            "x-api-key",
        ] {
            assert!(check_header(header).is_err(), "{header} was allowed");
        }
        assert!(check_header("content-type").is_ok());
        assert!(check_header("x-request-id").is_ok());
    }

    #[test]
    fn a_rule_the_api_actually_committed_to_is_provable() {
        let ws = uuid::Uuid::now_v7();
        let set = vec![rule("api.stripe.com"), rule("docs.example.com")];
        let committed = crate::egress::commit::root(ws, &set);
        assert!(vet_commitment(ws, &set, &committed, &set[0], "api.stripe.com").is_ok());
    }

    #[test]
    fn a_rule_the_runtime_invented_is_refused_even_though_it_is_in_the_list_it_holds() {
        // The list the runtime is carrying is not the thing being trusted --
        // the commitment is. So a rule that is genuinely in `self.egress` but
        // was never part of what the API hashed (a stale list, or one a
        // compromised host process altered) must still be refused: holding
        // the rule is not the same as being able to prove it.
        let ws = uuid::Uuid::now_v7();
        let committed_set = vec![rule("api.stripe.com")];
        let committed = crate::egress::commit::root(ws, &committed_set);

        let runtime_set = vec![rule("api.stripe.com"), rule("evil.example.com")];
        let invented = rule("evil.example.com");
        assert_eq!(
            vet_commitment(ws, &runtime_set, &committed, &invented, "evil.example.com"),
            Err(Refused::Unproven("evil.example.com".into()))
        );
    }

    #[test]
    fn an_absent_or_empty_commitment_refuses_rather_than_allows() {
        // "Could not verify" has to mean refused, never allowed. An empty
        // commitment is what a turn given no rules looks like, so a rule
        // checked against it -- whether the commitment was stripped, defaulted,
        // or simply never set -- must fail exactly the way a rule that was
        // never allowed fails.
        let ws = uuid::Uuid::now_v7();
        let set = vec![rule("api.stripe.com")];
        let empty = crate::egress::commit::empty_root();
        assert_eq!(
            vet_commitment(ws, &set, &empty, &set[0], "api.stripe.com"),
            Err(Refused::Unproven("api.stripe.com".into()))
        );
    }

    #[test]
    fn a_rule_from_the_wrong_workspace_does_not_verify_here() {
        // The commitment is per workspace, so a proof built correctly for one
        // workspace must not verify against another's turn -- otherwise one
        // workspace's rule list could be replayed to unlock hosts for a turn
        // that belongs to somebody else's.
        let mine = uuid::Uuid::now_v7();
        let yours = uuid::Uuid::now_v7();
        let set = vec![rule("api.stripe.com")];
        let mine_committed = crate::egress::commit::root(mine, &set);

        assert_eq!(
            vet_commitment(yours, &set, &mine_committed, &set[0], "api.stripe.com"),
            Err(Refused::Unproven("api.stripe.com".into()))
        );
    }

    #[test]
    fn a_credential_cannot_be_carried_across_by_forging_the_proof() {
        // Proving is not just "this host is allowed" -- it is "this exact
        // rule, credential and all, is the one the API vouched for". A rule
        // whose header or credential_env has been edited after the match, even
        // to a value some other genuine rule carries, must fail to verify.
        let ws = uuid::Uuid::now_v7();
        let set = vec![
            with_credential("api.stripe.com", "authorization", "STRIPE_KEY"),
            rule("docs.example.com"),
        ];
        let committed = crate::egress::commit::root(ws, &set);

        let swapped = with_credential("docs.example.com", "authorization", "STRIPE_KEY");
        assert_eq!(
            vet_commitment(ws, &set, &committed, &swapped, "docs.example.com"),
            Err(Refused::Unproven("docs.example.com".into()))
        );
    }
}
