//! A document fetched through the gateway, for the API's own needs -- a
//! specification for the wizard, the model catalog.
//!
//! Never with a client of the API's own, which would be a URL field that
//! reaches `outturn-api` and everything else in the cluster. The API has no
//! turn to borrow a token from, so it mints one for the request: committing to
//! the one host the URL names, with no credential, no gates, the
//! `document_fetch` role and a minute to live. The gateway then does what it
//! does for every request -- refuses a private address unless the operator
//! opened it, pins what it resolved, follows no redirect -- and the token is
//! dropped here. Unauthenticated by construction: the rule names no
//! credential, so none is attached.

use serde::Deserialize;
use uuid::Uuid;

use outturn_core::auth::TokenMinter;

/// What came back, as the gateway relays it.
#[derive(Debug, Deserialize)]
pub struct Fetched {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub truncated: bool,
}

impl Fetched {
    /// A response header, by name in any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("that URL is not one: {0}")]
    BadUrl(String),
    #[error("could not reach the gateway: {0}")]
    NoGateway(String),
    /// The gateway's refusals are written to be read: a private address, an
    /// unresolvable name, a host that did not answer.
    #[error("{0}")]
    Refused(String),
    #[error("the gateway's answer was unreadable: {0}")]
    Unreadable(String),
    #[error("{0}")]
    Internal(String),
}

/// GETs `url` through the gateway at `gateway`, with these request headers.
pub async fn get(
    minter: &TokenMinter,
    gateway: &str,
    actor: Uuid,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<Fetched, FetchError> {
    let parsed = reqwest::Url::parse(url).map_err(|e| FetchError::BadUrl(e.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(FetchError::BadUrl(
            "only http and https URLs are fetched".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| FetchError::BadUrl("it names no host".into()))?
        .to_string();

    let workspace = crate::usage::PLATFORM_WORKSPACE;
    let rules = vec![outturn_core::egress::rule::EgressRule {
        host,
        header: None,
        credential_env: None,
        client: None,
        credential: None,
    }];
    let gates = outturn_core::egress::gate::Gates::none();
    let token = minter
        .mint_document_fetch(
            actor,
            workspace,
            outturn_core::egress::commit::root(workspace, &rules),
            gates.root(workspace),
        )
        .map_err(|e| FetchError::Internal(e.to_string()))?;

    let response = outturn_core::http_client::reporting_client()
        .post(format!("{gateway}/v1/egress"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "method": "GET",
            "url": url,
            "gates": gates,
            "headers": headers,
            "proof": outturn_core::egress::commit::Proof::WholeSet { rules },
        }))
        .send()
        .await
        .map_err(|e| FetchError::NoGateway(e.to_string()))?;
    if !response.status().is_success() {
        return Err(FetchError::Refused(
            response.text().await.unwrap_or_default(),
        ));
    }
    response
        .json()
        .await
        .map_err(|e| FetchError::Unreadable(e.to_string()))
}
