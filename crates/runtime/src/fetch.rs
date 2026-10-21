//! Asking the gateway to make a request, which is the only way out of here.
//!
//! The runtime holds no credentials and, deliberately, no outbound HTTP path
//! of its own. Everything an agent reaches is reached by the gateway on its
//! behalf: that tier holds the secrets a legacy service wants in its own
//! header, and it can read the egress commitment out of the turn token, which
//! is the one thing about a turn this tier cannot rewrite.
//!
//! So what is here is a client and nothing else. No decision about what is
//! allowed is made in this file, and none should be added: a check here is a
//! check inside the sandbox's own host, which is the thing being defended
//! against rather than the thing doing the defending.

use serde::{Deserialize, Serialize};

use outturn_core::egress::commit;

/// What the gateway is being asked to send.
#[derive(Debug, Serialize)]
pub struct GatewayFetch {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// The rule this turn is using, and the API's word that it is genuine.
    pub proof: commit::Proof,
    /// What the API said gates this turn, passed on unchanged.
    ///
    /// Carried in full rather than as a proof, unlike the rule above: a rule is a
    /// permission this request wants and proves, while a gate is an obligation the
    /// gateway has to know all of before it can say a request matches none. This
    /// tier does not read it, choose it or check it -- it relays it, and the
    /// gateway checks it against the commitment in the turn token, so nothing
    /// here could usefully rewrite it.
    pub gates: outturn_core::egress::gate::Gates,
}

/// What came back.
#[derive(Debug, Deserialize)]
pub struct GatewayFetched {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub truncated: bool,
}

/// Why a request through the gateway brought back no answer to hand on.
#[derive(Debug)]
pub struct Unanswered {
    /// What the agent is told.
    pub message: String,
    /// Whether the request was sent, or may have been, with no answer back --
    /// so a write may have landed. Read from the gateway's failure header, or
    /// known here when the gateway itself could not be heard from.
    pub did_not_complete: bool,
    /// Whether the gateway said the request never left: the connection was not
    /// made, so nothing can have landed. Never true beside `did_not_complete`,
    /// and never assumed here -- a gateway this cannot hear from may have sent
    /// it.
    pub never_sent: bool,
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Unanswered {
    fn answered(message: String) -> Self {
        Unanswered {
            message,
            did_not_complete: false,
            never_sent: false,
        }
    }
}

/// Sends one request through the gateway.
///
/// A refusal comes back as the gateway's own words rather than being
/// reinterpreted here. They are written for whoever configured the rule, and
/// passing them through means the agent is told the same thing wherever the
/// decision was made.
pub async fn through_gateway(
    gateway_url: &str,
    token: &str,
    request: GatewayFetch,
) -> Result<GatewayFetched, Unanswered> {
    // The gateway holds the request open while it makes the outbound call, so
    // this waits on somebody else's upstream rather than on the gateway alone.
    let client =
        outturn_core::http_client::streaming_client(outturn_core::http_client::IDLE_TIMEOUT);
    let response = client
        .post(format!("{}/v1/egress", gateway_url.trim_end_matches('/')))
        .bearer_auth(token)
        .json(&request)
        .send()
        .await
        .map_err(|e| Unanswered {
            // Stripped, because this text reaches a model and a transcript: a
            // URL an agent composed can carry a credential in its query.
            message: format!(
                "the request did not complete: {}",
                strip_url(&e.to_string())
            ),
            did_not_complete: true,
            never_sent: false,
        })?;

    let status = response.status();
    if status.is_success() {
        return response
            .json::<GatewayFetched>()
            .await
            .map_err(|e| Unanswered::answered(format!("the response did not arrive whole: {e}")));
    }

    // The gateway's refusals are already written for a reader; what would be
    // unhelpful is a bare status where an explanation was sent.
    let mark = response
        .headers()
        .get(outturn_core::protocol::FAILURE_HEADER)
        .cloned();
    let did_not_complete = mark
        .as_ref()
        .is_some_and(|v| v == outturn_core::protocol::DID_NOT_COMPLETE);
    let never_sent = mark
        .as_ref()
        .is_some_and(|v| v == outturn_core::protocol::NOT_SENT);
    let detail = response.text().await.unwrap_or_default();
    Err(Unanswered {
        message: if detail.is_empty() {
            format!("the request was refused ({status})")
        } else {
            strip_url(&detail)
        },
        did_not_complete,
        never_sent,
    })
}

/// Removes URLs from a message before it is shown to a model.
fn strip_url(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| if word.contains("://") { "<url>" } else { word })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_never_carries_the_url_it_was_about() {
        let message = "the request did not complete: error for https://api.example.com/?key=sk-1";
        let stripped = strip_url(message);
        assert!(!stripped.contains("sk-1"), "{stripped}");
    }

    #[test]
    fn what_travels_is_the_proof_rather_than_the_rule_list() {
        // The gateway matches against what verified, so sending a workspace's
        // whole list would be sending something nobody reads. An inclusion
        // proof keeps a large list off every request.
        let workspace = uuid::Uuid::now_v7();
        let rules: Vec<outturn_core::egress::rule::EgressRule> = (0..50)
            .map(|i| outturn_core::egress::rule::EgressRule {
                host: format!("host{i}.example.com"),
                header: None,
                credential_env: None,
                client: None,
                credential: None,
            })
            .collect();
        let proof = commit::prove(workspace, &rules, &rules[0]).expect("in the set");

        let body = serde_json::to_string(&GatewayFetch {
            gates: outturn_core::egress::gate::Gates::none(),
            method: "GET".into(),
            url: "https://host0.example.com/".into(),
            headers: Vec::new(),
            body: None,
            proof,
        })
        .expect("serializes");

        assert!(
            !body.contains("host49.example.com"),
            "a fetch should not carry rules it is not using"
        );
    }
}
