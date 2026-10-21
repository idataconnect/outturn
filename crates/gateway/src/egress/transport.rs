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
    /// The connection was never made, so no byte of the request left: refused,
    /// unresolvable, or a handshake that failed. Nothing can have landed.
    NotSent(String),
    /// The request went, or may have, and no answer came back.
    Unreachable(String),
    /// It answered, and the answer did not arrive whole.
    Incomplete(String),
    /// The request could not be prepared. Ours, not theirs.
    Malformed(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::NotSent(detail) => {
                write!(f, "the request could not be sent: {detail}")
            }
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
        let client = client_for(&request.host, &request.addrs)?;

        let mut outgoing = client
            .request(request.method, request.url)
            .headers(request.headers)
            // Per request rather than on the client, so one client can serve
            // calls that were each given their own allowance.
            .timeout(request.timeout);
        if let Some(body) = request.body {
            outgoing = outgoing.body(body);
        }

        // Told apart by the step that failed. A connect error is raised before
        // the request is written, so nothing went; anything later -- a reset
        // mid-write, a timeout waiting on the answer -- may have landed, and
        // whoever decides whether to send it again has to know which.
        let response = outgoing.send().await.map_err(|e| {
            let detail = strip_url(&e.to_string());
            if e.is_connect() {
                TransportError::NotSent(detail)
            } else {
                TransportError::Unreachable(detail)
            }
        })?;

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

/// How many pinned clients to keep before starting over. A deployment reaches
/// as many hosts as its workspaces wrote rules for, and a host whose address
/// changes leaves its old client behind; the bound only has to stop that from
/// accumulating without end.
const CLIENTS_KEPT: usize = 256;

type Pin = (String, Vec<SocketAddr>);

/// Clients by what they are pinned to.
///
/// A client carries its connection pool, so building one per request threw
/// away every warm connection and paid a handshake each time. The pin is part
/// of what makes a client the same one, though: the same host resolving to
/// different addresses gets a different client, never the old pin.
#[derive(Default)]
struct Clients(std::sync::Mutex<std::collections::HashMap<Pin, reqwest::Client>>);

impl Clients {
    fn get(&self, host: &str, addrs: &[SocketAddr]) -> Result<reqwest::Client, TransportError> {
        let key: Pin = (host.to_string(), addrs.to_vec());
        let mut clients = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = clients.get(&key) {
            return Ok(client.clone());
        }
        let client = reqwest::Client::builder()
            .connect_timeout(outturn_core::http_client::CONNECT_TIMEOUT)
            // A redirect names a host that was never checked. Refusing to
            // follow is what keeps an allowed host from being a doorway.
            .redirect(reqwest::redirect::Policy::none())
            // Pinned to the answer the vetting already got. Letting the client
            // look the name up again would make the check and the connection
            // two different questions.
            .resolve_to_addrs(host, addrs)
            .build()
            .map_err(|e| TransportError::Malformed(e.to_string()))?;
        if clients.len() >= CLIENTS_KEPT {
            clients.clear();
        }
        clients.insert(key, client.clone());
        Ok(client)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

static CLIENTS: std::sync::LazyLock<Clients> = std::sync::LazyLock::new(Default::default);

fn client_for(host: &str, addrs: &[SocketAddr]) -> Result<reqwest::Client, TransportError> {
    CLIENTS.get(host, addrs)
}

/// Removes URLs from an error before it travels back.
///
/// A URL an agent built can carry a credential in its query string, and these
/// strings reach a model and a transcript that is read back on every later
/// turn.
pub(crate) fn strip_url(message: &str) -> String {
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

    fn vetted(addr: SocketAddr) -> VettedRequest {
        VettedRequest {
            method: reqwest::Method::POST,
            url: format!("http://held.example.com:{}/charge", addr.port())
                .parse()
                .unwrap(),
            host: "held.example.com".into(),
            addrs: vec![addr],
            headers: Default::default(),
            body: Some("{}".into()),
            timeout: Duration::from_secs(30),
            body_limit: 1024,
        }
    }

    /// A refused connection is one nothing was written to, and says so: it is
    /// what lets an approved call be sent again rather than handed to a person
    /// as an outcome nobody knows.
    #[tokio::test]
    async fn a_connection_never_made_is_not_sent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let failed = Direct
            .send(vetted(addr))
            .await
            .expect_err("nothing listens");
        assert!(matches!(failed, TransportError::NotSent(_)), "{failed:?}");
    }

    /// One that was accepted and then dropped before answering may have
    /// landed, so it is not called unsent.
    #[tokio::test]
    async fn a_request_read_and_never_answered_may_have_landed() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            // Dropped with the request read and nothing said.
        });
        let failed = Direct.send(vetted(addr)).await.expect_err("never answered");
        assert!(
            matches!(failed, TransportError::Unreachable(_)),
            "{failed:?}"
        );
        server.await.unwrap();
    }

    #[test]
    fn a_pinned_client_is_built_once_and_a_different_pin_gets_its_own() {
        let one: SocketAddr = "203.0.113.7:443".parse().unwrap();
        let two: SocketAddr = "203.0.113.8:443".parse().unwrap();
        let clients = Clients::default();
        clients.get("api.example.com", &[one]).unwrap();
        clients.get("api.example.com", &[one]).unwrap();
        assert_eq!(clients.len(), 1);
        // The same host answering from somewhere else is not the old pin.
        clients.get("api.example.com", &[two]).unwrap();
        assert_eq!(clients.len(), 2);
    }
}
