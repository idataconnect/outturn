//! A stand-in for the gateway, for tests that should not need a live model.
//!
//! Real providers are slow and nondeterministic: a long generation has been
//! measured at ~5 minutes, and the same prompt yields different text each run.
//! This serves scripted chunks over the same wire format, so the tiers above it
//! -- the component host, the worker, the event feed -- can be tested for
//! behaviour rather than for whatever a model happened to say.
//!
//! It deliberately does not validate tokens: it exists to exercise the callers,
//! and the gateway's own auth is covered against the real thing.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};

/// How the fake should answer the next request.
#[derive(Clone, Debug)]
pub enum Behaviour {
    /// Stream this text, split into chunks of a few characters.
    Reply(String),
    /// Stream some text, then drop the connection mid-generation.
    TruncateAfter { text: String, chunks: usize },
    /// Fail the request outright.
    Status(StatusCode, String),
    /// Hold the request open without responding, to exercise timeouts.
    Hang,
}

#[derive(Clone)]
pub struct FakeGateway {
    pub url: String,
    state: Arc<GatewayInner>,
}

struct GatewayInner {
    behaviour: Mutex<Behaviour>,
    /// Requests received, for asserting what the caller actually sent.
    seen: Mutex<Vec<serde_json::Value>>,
}

impl FakeGateway {
    /// Binds to an ephemeral port and serves until dropped.
    pub async fn start(behaviour: Behaviour) -> Self {
        let inner = Arc::new(GatewayInner {
            behaviour: Mutex::new(behaviour),
            seen: Mutex::new(Vec::new()),
        });

        let app = Router::new()
            .route("/v1/chat/completions", post(completions))
            .route("/v1/chat/completions/stream", post(completions_stream))
            .with_state(Arc::clone(&inner));

        // Port 0 lets the OS choose, so tests never contend for a fixed one.
        let listener = tokio::net::TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self {
            url: format!("http://{addr}"),
            state: inner,
        }
    }

    /// Changes what the next request receives.
    pub fn set(&self, behaviour: Behaviour) {
        *self.state.behaviour.lock().unwrap() = behaviour;
    }

    /// Requests received so far.
    pub fn requests(&self) -> Vec<serde_json::Value> {
        self.state.seen.lock().unwrap().clone()
    }
}

/// Splits text into small pieces, so a caller that only handles whole
/// responses is distinguishable from one that streams.
fn chunk_text(text: &str) -> Vec<String> {
    text.split_inclusive(' ')
        .map(str::to_string)
        .collect::<Vec<_>>()
}

fn chunk_json(text: &str, finish: Option<&str>) -> String {
    serde_json::json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "fake",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": text },
            "finish_reason": finish,
        }],
    })
    .to_string()
}

async fn completions_stream(
    State(state): State<Arc<GatewayInner>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    state.seen.lock().unwrap().push(request);
    let behaviour = state.behaviour.lock().unwrap().clone();

    match behaviour {
        Behaviour::Status(code, message) => (code, message).into_response(),

        Behaviour::Hang => {
            // Never resolves; the caller must impose its own deadline.
            std::future::pending::<()>().await;
            unreachable!()
        }

        Behaviour::Reply(text) => {
            let mut lines: Vec<String> = chunk_text(&text)
                .iter()
                .map(|piece| format!("{}\n", chunk_json(piece, None)))
                .collect();
            lines.push(format!("{}\n", chunk_json("", Some("stop"))));
            ndjson(lines)
        }

        Behaviour::TruncateAfter { text, chunks } => {
            // Ends without a finish_reason, as a dropped upstream would.
            let lines: Vec<String> = chunk_text(&text)
                .iter()
                .take(chunks)
                .map(|piece| format!("{}\n", chunk_json(piece, None)))
                .collect();
            ndjson(lines)
        }
    }
}

async fn completions(
    State(state): State<Arc<GatewayInner>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    state.seen.lock().unwrap().push(request);
    let behaviour = state.behaviour.lock().unwrap().clone();

    let text = match behaviour {
        Behaviour::Reply(text) => text,
        Behaviour::TruncateAfter { text, .. } => text,
        Behaviour::Status(code, message) => return (code, message).into_response(),
        Behaviour::Hang => {
            std::future::pending::<()>().await;
            unreachable!()
        }
    };

    Json(serde_json::json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "created": 0,
        "model": "fake",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
    }))
    .into_response()
}

fn ndjson(lines: Vec<String>) -> Response {
    let stream = futures::stream::iter(
        lines
            .into_iter()
            .map(|line| Ok::<_, std::io::Error>(axum::body::Bytes::from(line))),
    );
    (
        [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response()
}
