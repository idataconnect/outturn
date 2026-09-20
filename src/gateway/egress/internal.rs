//! Hosts inside the network that an operator has said may be reached.
//!
//! Everything private is refused by default, which is right for a workspace
//! that could otherwise aim the gateway at `outturn-api`, and wrong for the
//! ordinary case: most deployments want an agent to call a service the customer
//! runs, and most of the time that service runs in the same cluster.
//!
//! So an operator names the exceptions. Not a workspace -- a workspace allowing
//! `tickets.internal` is saying what its agents need, and a workspace allowing
//! `outturn-api` is attacking the platform, and only somebody outside the
//! workspace can tell those apart. See `docs/egress.md`.

use std::net::SocketAddr;

/// One host an operator has allowed, and optionally the one port on it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// As written. A name is compared as a name and an address as an address:
    /// allowlisting a name trusts DNS to keep pointing where you expect, and
    /// allowlisting an address trusts nothing, so the two are never the same
    /// entry.
    host: String,
    /// `None` permits any port on that host.
    port: Option<u16>,
}

/// What an operator has opened, in the order they wrote it.
///
/// Empty is the default and means the private-address refusal stands
/// everywhere, which is what a deployment that has not thought about this
/// should get.
#[derive(Debug, Clone, Default)]
pub struct Internal {
    entries: Vec<Entry>,
}

impl Internal {
    /// Parses a comma or whitespace separated list.
    ///
    /// Unparseable entries are dropped with a warning rather than failing the
    /// process. A gateway that refused to start over one malformed host would
    /// turn a typo into an outage, and the failure it leaves instead -- that
    /// host is refused like any other private address -- is the safe direction
    /// and says so in a log line.
    pub fn parse(raw: &str) -> Self {
        let mut entries = Vec::new();
        for token in raw
            .split([',', ' ', '\t', '\n'])
            .filter(|t| !t.trim().is_empty())
        {
            match Entry::parse(token.trim()) {
                Some(entry) => entries.push(entry),
                None => tracing::warn!(
                    entry = token.trim(),
                    "an internal-host entry could not be read and was ignored"
                ),
            }
        }
        Self { entries }
    }

    pub fn from_env() -> Self {
        let list = Self::parse(&std::env::var("OUTTURN_INTERNAL_HOSTS").unwrap_or_default());
        if !list.is_empty() {
            tracing::info!(
                hosts = list.entries.len(),
                "internal hosts an operator has opened to agents and routes"
            );
        }
        list
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether this host and port were opened.
    ///
    /// The host is compared as the caller wrote it, because that is what the
    /// operator wrote too. A bare entry permits any port; one naming a port
    /// permits only that port, so `tickets.internal:8080` opens the ticketing
    /// API and not the database beside it.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let host = host.trim_matches(['[', ']']);
        self.entries.iter().any(|e| {
            e.host.eq_ignore_ascii_case(host) && e.port.is_none_or(|allowed| allowed == port)
        })
    }

    /// Whether every address a name resolved to is one this entry covers.
    ///
    /// For a name, the entry is about the name and the addresses behind it are
    /// whatever DNS said -- so this is not consulted. For a literal address the
    /// two are the same thing, and checking both would be checking twice.
    pub fn allows_resolved(&self, host: &str, addrs: &[SocketAddr]) -> bool {
        !addrs.is_empty() && addrs.iter().all(|addr| self.allows(host, addr.port()))
    }
}

impl Entry {
    fn parse(raw: &str) -> Option<Self> {
        // A bracketed v6 literal carries colons of its own, so the port is
        // whatever follows the closing bracket -- `[::1]:8080` is a host and a
        // port, and `::1` is a host.
        if let Some(rest) = raw.strip_prefix('[') {
            let (host, tail) = rest.split_once(']')?;
            let port = match tail {
                "" => None,
                p => Some(p.strip_prefix(':')?.parse().ok()?),
            };
            return (!host.is_empty()).then(|| Entry {
                host: host.to_string(),
                port,
            });
        }

        // An unbracketed v6 literal has several colons and no port; anything
        // else with one colon has a port after it.
        if raw.matches(':').count() > 1 {
            return (!raw.is_empty()).then(|| Entry {
                host: raw.to_string(),
                port: None,
            });
        }

        match raw.split_once(':') {
            Some((host, port)) => {
                let port: u16 = port.parse().ok()?;
                (!host.is_empty() && port != 0).then(|| Entry {
                    host: host.to_string(),
                    port: Some(port),
                })
            }
            None => (!raw.is_empty()).then(|| Entry {
                host: raw.to_string(),
                port: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_allowed_by_default() {
        let list = Internal::default();
        assert!(list.is_empty());
        assert!(!list.allows("tickets.internal", 8080));
    }

    #[test]
    fn a_bare_host_allows_any_port_on_it() {
        let list = Internal::parse("tickets.internal");
        assert!(list.allows("tickets.internal", 8080));
        assert!(list.allows("tickets.internal", 443));
    }

    #[test]
    fn a_host_with_a_port_allows_only_that_port() {
        // The point of naming one: the database beside the API on the same
        // host is not what anybody meant to open.
        let list = Internal::parse("tickets.internal:8080");
        assert!(list.allows("tickets.internal", 8080));
        assert!(!list.allows("tickets.internal", 5432));
    }

    #[test]
    fn a_host_nobody_named_is_not_allowed() {
        let list = Internal::parse("tickets.internal:8080");
        assert!(!list.allows("outturn-api", 8080));
    }

    #[test]
    fn a_literal_address_is_an_entry_like_any_other() {
        let list = Internal::parse("172.18.0.1:11434");
        assert!(list.allows("172.18.0.1", 11434));
        assert!(!list.allows("172.18.0.1", 8080));
    }

    #[test]
    fn a_name_and_the_address_behind_it_are_separate_entries() {
        // Allowlisting a name trusts DNS to keep pointing where you expect;
        // allowlisting an address trusts nothing. One does not imply the other.
        let list = Internal::parse("tickets.internal");
        assert!(!list.allows("10.1.2.3", 8080));
    }

    #[test]
    fn commas_spaces_and_newlines_all_separate() {
        let list = Internal::parse("a.internal:1, b.internal:2\n c.internal:3");
        assert!(list.allows("a.internal", 1));
        assert!(list.allows("b.internal", 2));
        assert!(list.allows("c.internal", 3));
    }

    #[test]
    fn a_v6_literal_keeps_its_colons() {
        let list = Internal::parse("[fd00::1]:8080");
        assert!(list.allows("fd00::1", 8080));
        assert!(!list.allows("fd00::1", 9090));

        let bare = Internal::parse("fd00::1");
        assert!(bare.allows("fd00::1", 8080));
    }

    #[test]
    fn a_host_is_matched_without_regard_to_case() {
        // DNS is case-insensitive, and an operator who typed one case should
        // not be surprised by a caller that used the other.
        let list = Internal::parse("Tickets.Internal:8080");
        assert!(list.allows("tickets.internal", 8080));
    }

    #[test]
    fn something_unreadable_is_dropped_rather_than_refusing_to_start() {
        // A typo becomes one host that is not open, not an outage.
        let list = Internal::parse("good.internal:8080, bad.internal:not-a-port, other.internal");
        assert!(list.allows("good.internal", 8080));
        assert!(list.allows("other.internal", 1));
        assert!(!list.allows("bad.internal", 8080));
    }

    #[test]
    fn port_zero_is_not_a_port() {
        assert!(Entry::parse("host.internal:0").is_none());
    }

    #[test]
    fn every_address_behind_a_name_has_to_be_covered() {
        let list = Internal::parse("many.internal:8080");
        let ok = [
            "10.0.0.1:8080".parse().unwrap(),
            "10.0.0.2:8080".parse().unwrap(),
        ];
        assert!(list.allows_resolved("many.internal", &ok));

        // One address on a port nobody opened is enough to refuse the lot:
        // the connection could be made to any of them.
        let mixed = [
            "10.0.0.1:8080".parse().unwrap(),
            "10.0.0.2:5432".parse().unwrap(),
        ];
        assert!(!list.allows_resolved("many.internal", &mixed));

        assert!(!list.allows_resolved("many.internal", &[]));
    }
}
