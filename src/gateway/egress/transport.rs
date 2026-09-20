//! Where a vetted request actually leaves from.
//!
//! Everything about *whether* a request may go out is settled before anything
//! here is called: the rule was proven against what the API committed to, the
//! address was resolved and vetted, the credential was attached. What is left
//! is the sending, and that is the one part a deployment might reasonably want
//! to do differently.
//!
//! The open-source answer is `Direct`: the gateway pod makes the request
//! itself, from whatever address it has. That is the right default and, for
//! most people, the only one they will ever need.
//!
//! It is a trait because the alternatives are real and none of them belong
//! here. An operator whose upstreams rate-limit by source address wants the
//! fleet's traffic spread across a pool rather than arriving from one pod; one
//! serving several regions wants an exit close to the service; one inside an
//! enterprise network wants a forward proxy that already exists. Each of those
//! is somebody's infrastructure rather than this project's, so what this
//! offers is the seam and one honest implementation behind it.
//!
//! The seam is deliberately *after* the vetting rather than around it. A
//! transport receives a request whose address has already been decided and
//! cannot choose a different one, so an implementation swapped in here can
//! change where traffic leaves from without being able to widen what may be
//! reached.

use std::net::SocketAddr;
use std::time::Duration;

/// A request that has already been allowed, with nothing left to decide.
///
/// `addrs` is the resolved, vetted answer, and the transport is expected to
/// connect to it rather than resolve the host again. Resolving twice is how a
/// name that passed the check becomes a connection to somewhere else.
#[derive(Debug, Clone)]
pub struct VettedRequest {
    pub method: reqwest::Method,
    pub url: reqwest::Url,
    /// The host as it will be sent, kept beside the addresses it resolved to
    /// so a transport can pin one to the other.
    pub host: String,
    pub addrs: Vec<SocketAddr>,
    pub headers: reqwest::header::HeaderMap,
    pub body: Option<String>,
    pub timeout: Duration,
    /// How much of the response to keep. A body is written by somebody else,
    /// and one that never ends is a way to exhaust whoever is reading it.
    pub body_limit: usize,
}

/// What came back, already bounded.
#[derive(Debug, Clone)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Whether the body was cut short at `body_limit`, so the agent can be
    /// told rather than left to wonder why a document ends mid-sentence.
    pub truncated: bool,
}

/// Why a request did not complete.
///
/// Separate from a refusal: nothing here means "not allowed", because that was
/// decided before a transport was reached. These are the ways a permitted
/// request fails, and they are what the breaker reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The endpoint could not be reached at all.
    Unreachable(String),
    /// It answered, and the answer did not arrive whole.
    Incomplete(String),
    /// The request could not be prepared. Ours, not theirs.
    Malformed(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Unreachable(detail) => {
                write!(f, "the request did not complete: {detail}")
            }
            TransportError::Incomplete(detail) => {
                write!(f, "the response did not arrive whole: {detail}")
            }
            TransportError::Malformed(detail) => {
                write!(f, "could not prepare the request: {detail}")
            }
        }
    }
}

impl std::error::Error for TransportError {}

/// Sends a request that has already been allowed.
#[async_trait::async_trait]
pub trait EgressTransport: Send + Sync {
    async fn send(&self, request: VettedRequest) -> Result<TransportResponse, TransportError>;

    /// What to call this in a log line. Operators need to know which way their
    /// traffic left when they are working out why an upstream saw what it saw.
    fn name(&self) -> &'static str;
}

/// Straight out of this pod, which is what the open-source deployment does.
pub struct Direct;

#[async_trait::async_trait]
impl EgressTransport for Direct {
    fn name(&self) -> &'static str {
        "direct"
    }

    async fn send(&self, request: VettedRequest) -> Result<TransportResponse, TransportError> {
        let client = reqwest::Client::builder()
            .connect_timeout(crate::http_client::CONNECT_TIMEOUT)
            .timeout(request.timeout)
            // A redirect names a host that was never checked. Refusing to
            // follow is what keeps an allowed host from being a doorway.
            .redirect(reqwest::redirect::Policy::none())
            // Pinned to the answer the vetting already got. Letting the client
            // look the name up again would make the check and the connection
            // two different questions.
            .resolve_to_addrs(&request.host, &request.addrs)
            .build()
            .map_err(|e| TransportError::Malformed(e.to_string()))?;

        let mut outgoing = client
            .request(request.method, request.url)
            .headers(request.headers);
        if let Some(body) = request.body {
            outgoing = outgoing.body(body);
        }

        let response = outgoing
            .send()
            .await
            .map_err(|e| TransportError::Unreachable(strip_url(&e.to_string())))?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();

        // Read to a bound rather than to the end, taking chunks until the
        // limit is passed and then dropping the connection, so what is held is
        // never more than one chunk over the bound.
        let mut body: Vec<u8> = Vec::new();
        let mut truncated = false;
        {
            use futures::StreamExt;
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk =
                    chunk.map_err(|e| TransportError::Incomplete(strip_url(&e.to_string())))?;
                body.extend_from_slice(&chunk);
                if body.len() > request.body_limit {
                    truncated = true;
                    body.truncate(request.body_limit);
                    break;
                }
            }
        }

        Ok(TransportResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&body).to_string(),
            truncated,
        })
    }
}

/// Removes URLs from an error before it travels back.
///
/// A URL an agent built can carry a credential in its query string, and these
/// strings reach a model and a transcript that is read back on every later
/// turn.
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
    fn an_error_never_carries_the_url_it_was_about() {
        // These strings reach a model and are replayed on every later turn, and
        // a URL an agent composed can carry a credential in its query string.
        let message = "error sending request for url (https://api.example.com/v1?key=sk-secret)";
        let stripped = strip_url(message);
        assert!(!stripped.contains("sk-secret"), "{stripped}");
        assert!(!stripped.contains("api.example.com"), "{stripped}");
    }

    #[test]
    fn a_transport_error_says_which_kind_of_failure_it_was() {
        // The breaker reads these: unreachable is evidence about the service,
        // and a malformed request is ours rather than theirs.
        assert!(
            TransportError::Unreachable("connection refused".into())
                .to_string()
                .contains("did not complete")
        );
        assert!(
            TransportError::Malformed("bad header".into())
                .to_string()
                .contains("could not prepare")
        );
    }
}
