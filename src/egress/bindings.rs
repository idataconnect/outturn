//! Which workspaces may use a credential variable, and where it may be sent.
//!
//! A rule names the variable its credential comes from, and every workspace
//! writes its own rules, so the `OUTTURN_EGRESS_` namespace is shared: without
//! this, one workspace could name a variable the operator set up for another
//! and aim it at a host of its own. A binding is the operator's statement of
//! who may use a variable and where it goes, kept beside the secret on the
//! gateway rather than anywhere a workspace -- or the API -- can write. See
//! `docs/credential-bindings.md`.
//!
//! Unbound means refused, and a declaration that cannot be read binds nothing.
//! Both are the same direction the commitment keeps: "could not verify" has to
//! mean refused.

use std::collections::HashMap;

use uuid::Uuid;

use crate::runtime::egress::{ClientCredentials, EgressRule};

/// Where the operator writes them. Deliberately outside `OUTTURN_EGRESS_`, so
/// no rule can name the bindings as its credential.
pub const VARIABLE: &str = "OUTTURN_CREDENTIAL_BINDINGS";

/// One variable's binding, as the operator wrote it.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    /// Workspace ids, or `"*"` for every workspace. The hosts are what keep a
    /// secret from leaving for somewhere the operator did not name; this list
    /// only says who may use it at those hosts, and a key every workspace
    /// shares -- one booking API for all of them -- is honestly `"*"`.
    workspaces: Vec<String>,
    /// Exact names, never wildcards: a wildcard binding would bind the secret
    /// to hosts nobody has seen.
    hosts: Vec<String>,
    /// For a client-credentials variable, the one endpoint it is exchanged at.
    #[serde(default)]
    token_url: Option<String>,
}

#[derive(Debug, Clone)]
struct Binding {
    /// `None` for every workspace.
    workspaces: Option<Vec<Uuid>>,
    hosts: Vec<String>,
    token_url: Option<reqwest::Url>,
}

/// Every variable the operator has bound.
///
/// Empty is the default, and binds nothing: a deployment that has not written
/// any attaches no credential to anything.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    by_variable: HashMap<String, Binding>,
}

impl Bindings {
    /// Reads a declaration, refusing all of it if any of it is wrong.
    ///
    /// Not entry by entry, as the internal-hosts list does it. Dropping a bad
    /// internal host leaves that host refused, which is safe; dropping a bad
    /// field from a binding could leave the rest of it wider than meant -- a
    /// misspelt `token_url` would bind the variable to its hosts with no
    /// endpoint at all. So a declaration is read whole or not at all.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        let entries: HashMap<String, Entry> =
            serde_json::from_str(raw).map_err(|e| format!("{VARIABLE} is not readable: {e}"))?;
        let mut by_variable = HashMap::new();
        for (variable, entry) in entries {
            crate::runtime::egress::check_credential_variable(&variable)?;
            let binding = Binding::read(&variable, entry)?;
            by_variable.insert(variable, binding);
        }
        Ok(Self { by_variable })
    }

    /// The operator's bindings, read once at startup.
    ///
    /// A declaration that cannot be read binds nothing, and says so as loudly
    /// as this process can short of refusing to start: every credentialed
    /// request is about to be refused, and this line is why. Refusing to start
    /// would turn one typo into an outage of every model call as well.
    pub fn from_env() -> Self {
        let raw = std::env::var(VARIABLE).unwrap_or_default();
        let bindings = match Self::parse(&raw) {
            Ok(bindings) => bindings,
            Err(why) => {
                tracing::error!(
                    error = %why,
                    "credential bindings could not be read, so no credential is bound \
                     and every request carrying one will be refused"
                );
                Self::default()
            }
        };
        bindings.report_unbound(std::env::vars().map(|(name, _)| name));
        bindings
    }

    /// Names every variable under the prefix that nothing binds, so an upgrade
    /// is a list to read rather than a refusal to trace. Names only.
    fn report_unbound(&self, names: impl Iterator<Item = String>) {
        let mut unbound: Vec<String> = names
            .filter(|n| n.starts_with(crate::runtime::egress::CREDENTIAL_PREFIX))
            .filter(|n| !self.by_variable.contains_key(n))
            .collect();
        unbound.sort();
        if !unbound.is_empty() {
            tracing::warn!(
                variables = %unbound.join(","),
                "credential variables with no binding; no rule can use them"
            );
        }
        if !self.by_variable.is_empty() {
            tracing::info!(
                variables = self.by_variable.len(),
                "credential bindings read"
            );
        }
    }

    /// Whether `workspace` may send `variable` to `host`.
    pub fn check_static(&self, workspace: Uuid, variable: &str, host: &str) -> Result<(), String> {
        self.binding_for(workspace, variable, host).map(|_| ())
    }

    /// Whether `workspace` may exchange this pair at its token URL and send
    /// the token to `host`. Both variables must bind the same endpoint, so a
    /// pair cannot be assembled from halves bound to different places.
    pub fn check_client(
        &self,
        workspace: Uuid,
        client: &ClientCredentials,
        host: &str,
    ) -> Result<(), String> {
        let asked = reqwest::Url::parse(&client.token_url)
            .map_err(|_| format!("{} is not a URL", client.token_url))?;
        for variable in [&client.client_id_env, &client.client_secret_env] {
            let binding = self.binding_for(workspace, variable, host)?;
            if binding.token_url.as_ref() != Some(&asked) {
                return Err(format!(
                    "{variable} is not bound to the token endpoint {asked}, so it is not sent there"
                ));
            }
        }
        Ok(())
    }

    /// Whether a rule could ever attach its credential for `workspace`: for a
    /// rule whose host is a wildcard, whether any bound host falls under it.
    /// For the API to answer while somebody is looking; the gateway asks of
    /// each request instead.
    pub fn check_rule(&self, workspace: Uuid, rule: &EgressRule) -> Result<(), String> {
        let host = |b: &Binding| {
            b.hosts
                .iter()
                .find(|h| crate::runtime::egress::host_matches(&rule.host, h))
                .cloned()
        };
        if let Some(variable) = &rule.credential_env {
            let found = self.bound_to(workspace, variable)?;
            let host = host(found).ok_or_else(|| not_toward(variable, &rule.host))?;
            self.check_static(workspace, variable, &host)?;
        }
        if let Some(client) = &rule.client {
            let found = self.bound_to(workspace, &client.client_id_env)?;
            let host = host(found).ok_or_else(|| not_toward(&client.client_id_env, &rule.host))?;
            self.check_client(workspace, client, &host)?;
        }
        Ok(())
    }

    fn bound_to(&self, workspace: Uuid, variable: &str) -> Result<&Binding, String> {
        let binding = self.by_variable.get(variable).ok_or_else(|| {
            format!("{variable} is not bound to any workspace, so no rule can use it")
        })?;
        match &binding.workspaces {
            Some(list) if !list.contains(&workspace) => Err(format!(
                "{variable} is not bound to this workspace, so it is not sent"
            )),
            _ => Ok(binding),
        }
    }

    fn binding_for(&self, workspace: Uuid, variable: &str, host: &str) -> Result<&Binding, String> {
        let binding = self.bound_to(workspace, variable)?;
        let host = host.trim_matches(['[', ']']);
        if !binding.hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return Err(not_toward(variable, host));
        }
        Ok(binding)
    }
}

fn not_toward(variable: &str, host: &str) -> String {
    format!("{variable} is not bound to {host}, so it is not sent there")
}

impl Binding {
    fn read(variable: &str, entry: Entry) -> Result<Self, String> {
        let workspaces = if entry.workspaces.iter().any(|w| w == "*") {
            if entry.workspaces.len() != 1 {
                return Err(format!(
                    "{variable}: \"*\" already means every workspace; list ids or \"*\", not both"
                ));
            }
            None
        } else if entry.workspaces.is_empty() {
            return Err(format!("{variable} is bound to no workspace"));
        } else {
            let ids = entry
                .workspaces
                .iter()
                .map(|w| {
                    Uuid::parse_str(w)
                        .map_err(|_| format!("{variable}: {w:?} is not a workspace id"))
                })
                .collect::<Result<_, _>>()?;
            Some(ids)
        };

        if entry.hosts.is_empty() {
            return Err(format!("{variable} is bound to no host"));
        }
        let mut hosts = Vec::new();
        for host in entry.hosts {
            let host = host.trim().to_ascii_lowercase();
            if host.is_empty() || host.contains(['*', '/', ':', ' ']) {
                return Err(format!(
                    "{variable}: {host:?} is not a host name. A binding names exact hosts, \
                     without a scheme, port or wildcard"
                ));
            }
            hosts.push(host);
        }

        let token_url = entry
            .token_url
            .map(|u| reqwest::Url::parse(&u).map_err(|_| format!("{variable}: {u:?} is not a URL")))
            .transpose()?;

        Ok(Self {
            workspaces,
            hosts,
            token_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::egress::ClientAuth;

    const ACME: &str = "01920000-0000-7000-8000-00000000000a";
    const OTHER: &str = "01920000-0000-7000-8000-00000000000b";

    fn acme() -> Uuid {
        Uuid::parse_str(ACME).unwrap()
    }
    fn other() -> Uuid {
        Uuid::parse_str(OTHER).unwrap()
    }

    fn bindings() -> Bindings {
        Bindings::parse(&format!(
            r#"{{
                "OUTTURN_EGRESS_ACME_STRIPE": {{"workspaces": ["{ACME}"], "hosts": ["api.stripe.com"]}},
                "OUTTURN_EGRESS_BOOKING": {{"workspaces": ["*"], "hosts": ["booking.example.com"]}},
                "OUTTURN_EGRESS_LEDGER_ID": {{"workspaces": ["{ACME}"], "hosts": ["ledger.example.com"],
                    "token_url": "https://auth.example.com/token"}},
                "OUTTURN_EGRESS_LEDGER_SECRET": {{"workspaces": ["{ACME}"], "hosts": ["ledger.example.com"],
                    "token_url": "https://auth.example.com/token"}}
            }}"#
        ))
        .expect("readable")
    }

    fn client(token_url: &str) -> ClientCredentials {
        ClientCredentials {
            token_url: token_url.into(),
            scope: None,
            client_id_env: "OUTTURN_EGRESS_LEDGER_ID".into(),
            client_secret_env: "OUTTURN_EGRESS_LEDGER_SECRET".into(),
            client_auth: ClientAuth::Basic,
        }
    }

    #[test]
    fn a_variable_goes_to_its_workspace_at_its_host() {
        let b = bindings();
        assert!(
            b.check_static(acme(), "OUTTURN_EGRESS_ACME_STRIPE", "api.stripe.com")
                .is_ok()
        );
        assert!(
            b.check_static(acme(), "OUTTURN_EGRESS_ACME_STRIPE", "API.Stripe.com")
                .is_ok()
        );
    }

    /// The leak this exists to close.
    #[test]
    fn another_workspace_cannot_use_it() {
        let why = bindings()
            .check_static(other(), "OUTTURN_EGRESS_ACME_STRIPE", "api.stripe.com")
            .unwrap_err();
        assert!(why.contains("not bound to this workspace"), "{why}");
    }

    #[test]
    fn its_own_workspace_cannot_send_it_elsewhere() {
        let why = bindings()
            .check_static(acme(), "OUTTURN_EGRESS_ACME_STRIPE", "collect.example.net")
            .unwrap_err();
        assert!(why.contains("not bound to collect.example.net"), "{why}");
    }

    #[test]
    fn every_workspace_may_use_a_shared_key_but_only_at_its_host() {
        let b = bindings();
        assert!(
            b.check_static(other(), "OUTTURN_EGRESS_BOOKING", "booking.example.com")
                .is_ok()
        );
        assert!(
            b.check_static(other(), "OUTTURN_EGRESS_BOOKING", "evil.example.com")
                .is_err()
        );
    }

    #[test]
    fn an_unbound_variable_goes_nowhere() {
        let why = bindings()
            .check_static(acme(), "OUTTURN_EGRESS_NOBODY", "api.stripe.com")
            .unwrap_err();
        assert!(why.contains("not bound to any workspace"), "{why}");
    }

    #[test]
    fn a_secret_is_exchanged_only_at_its_endpoint() {
        let b = bindings();
        assert!(
            b.check_client(
                acme(),
                &client("https://auth.example.com/token"),
                "ledger.example.com"
            )
            .is_ok()
        );
        let why = b
            .check_client(
                acme(),
                &client("https://attacker.example/token"),
                "ledger.example.com",
            )
            .unwrap_err();
        assert!(why.contains("token endpoint"), "{why}");
        assert!(
            b.check_client(
                other(),
                &client("https://auth.example.com/token"),
                "ledger.example.com"
            )
            .is_err()
        );
    }

    /// A pair whose halves name different endpoints is not a pair.
    #[test]
    fn a_static_variable_is_not_a_client_secret() {
        let mut c = client("https://auth.example.com/token");
        c.client_secret_env = "OUTTURN_EGRESS_ACME_STRIPE".into();
        assert!(
            bindings()
                .check_client(acme(), &c, "ledger.example.com")
                .is_err()
        );
    }

    #[test]
    fn a_wildcard_rule_is_bound_if_a_bound_host_falls_under_it() {
        let rule = |host: &str| EgressRule {
            host: host.into(),
            header: Some("authorization".into()),
            credential_env: Some("OUTTURN_EGRESS_ACME_STRIPE".into()),
            client: None,
        };
        let b = bindings();
        assert!(b.check_rule(acme(), &rule("*.stripe.com")).is_ok());
        assert!(b.check_rule(acme(), &rule("*.example.net")).is_err());
        assert!(b.check_rule(other(), &rule("api.stripe.com")).is_err());
    }

    /// Anything wrong refuses the whole declaration, which `from_env` turns
    /// into nothing bound.
    #[test]
    fn a_declaration_with_anything_wrong_is_refused_whole() {
        for raw in [
            "not json",
            r#"{"OUTTURN_EGRESS_A": {"workspaces": ["*"], "hosts": ["a.example.com"], "tokn_url": "x"}}"#,
            r#"{"OUTTURN_EGRESS_A": {"workspaces": ["*"], "hosts": ["*.example.com"]}}"#,
            r#"{"OUTTURN_EGRESS_A": {"workspaces": ["*"], "hosts": ["a.example.com:443"]}}"#,
            r#"{"OUTTURN_EGRESS_A": {"workspaces": ["*"], "hosts": []}}"#,
            r#"{"OUTTURN_EGRESS_A": {"workspaces": [], "hosts": ["a.example.com"]}}"#,
            r#"{"OUTTURN_EGRESS_A": {"workspaces": ["acme"], "hosts": ["a.example.com"]}}"#,
            r#"{"OUTTURN_EGRESS_A": {"workspaces": ["*", "01920000-0000-7000-8000-00000000000a"], "hosts": ["a.example.com"]}}"#,
            r#"{"DATABASE_URL": {"workspaces": ["*"], "hosts": ["a.example.com"]}}"#,
        ] {
            assert!(Bindings::parse(raw).is_err(), "{raw} was accepted");
        }
        assert!(Bindings::parse("").unwrap().by_variable.is_empty());
    }
}
