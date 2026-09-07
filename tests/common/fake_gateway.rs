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
    /// Ask for a tool, and report a message the user sent mid-turn, then
    /// answer. Stands in for the gateway noticing pending input.
    ToolThenSteer {
        name: String,
        arguments: String,
        steer: String,
        reply: String,
    },
    /// Answer in prose, with a message the user sent mid-turn appended, then
    /// answer that message in prose on the next call. Two rounds of text and
    /// no tools, which is what a steer during an ordinary reply looks like.
    TextThenSteer {
        first: String,
        steer: String,
        reply: String,
    },
    /// Ask for a tool in a reply that was cut off at the token limit, so the
    /// arguments cannot be trusted.
    TruncatedToolCall { name: String, arguments: String },
    /// Ask for a tool on every request, so only a limit ends the loop.
    AlwaysToolCall {
        name: String,
        arguments: String,
        /// Said alongside the tool call, so a bounded turn has something to
        /// show rather than ending empty.
        content: String,
    },
    /// Ask for a tool on the first request, then answer with text.
    ///
    /// Two phases because that is what a tool loop is: the model asks, the
    /// guest runs the tool, the model answers with the result in hand.
    ToolThenReply {
        name: String,
        /// Sent split across chunks, as a provider does, so a caller that
        /// fails to reassemble the fragments is caught.
        arguments: String,
        reply: String,
    },
}

#[derive(Clone)]
pub struct FakeGateway {
    pub url: String,
    state: Arc<GatewayInner>,
}

struct GatewayInner {
    behaviour: Mutex<Behaviour>,
    /// Requests answered so far, so a two-phase behaviour knows which turn
    /// of the loop it is serving.
    calls: Mutex<usize>,
    /// Requests received, for asserting what the caller actually sent.
    seen: Mutex<Vec<serde_json::Value>>,
}

impl FakeGateway {
    /// Binds to an ephemeral port and serves until dropped.
    pub async fn start(behaviour: Behaviour) -> Self {
        let inner = Arc::new(GatewayInner {
            behaviour: Mutex::new(behaviour),
            calls: Mutex::new(0),
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
    let mut chunk = serde_json::json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "fake",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": text },
            "finish_reason": finish,
        }],
    });
    // Providers report usage on the final chunk, so the fake does too --
    // otherwise nothing here would exercise the accounting.
    if finish.is_some() {
        // With the breakdown, so the split is exercised rather than assumed:
        // this protocol folds cached tokens into the prompt total, and the
        // reader is expected to take them back out.
        chunk["usage"] = serde_json::json!({
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18,
            "prompt_tokens_details": { "cached_tokens": 4, "cache_creation_tokens": 2 },
            "completion_tokens_details": { "reasoning_tokens": 3 },
        });
    }
    chunk.to_string()
}

/// One chunk of a tool call being streamed in pieces.
fn tool_chunk(id: Option<&str>, name: Option<&str>, arguments: &str) -> String {
    let mut call = serde_json::json!({ "index": 0 });
    if let Some(id) = id {
        call["id"] = serde_json::json!(id);
        call["type"] = serde_json::json!("function");
    }
    let mut function = serde_json::json!({ "arguments": arguments });
    if let Some(name) = name {
        function["name"] = serde_json::json!(name);
    }
    call["function"] = function;

    serde_json::json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "fake",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "tool_calls": [call] },
            "finish_reason": null,
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
    let call_number = {
        let mut calls = state.calls.lock().unwrap();
        *calls += 1;
        *calls
    };

    match behaviour {
        Behaviour::ToolThenSteer {
            name,
            arguments,
            steer,
            reply,
        } => {
            if call_number > 1 {
                let mut lines: Vec<String> = chunk_text(&reply)
                    .iter()
                    .map(|piece| format!("{}\n", chunk_json(piece, None)))
                    .collect();
                lines.push(format!("{}\n", chunk_json("", Some("stop"))));
                return ndjson(lines);
            }

            let mut lines = vec![format!(
                "{}\n",
                tool_chunk(Some("call_steer"), Some(&name), &arguments)
            )];
            lines.push(format!("{}\n", chunk_json("", Some("tool_calls"))));
            // The trailing control line the real gateway appends.
            lines.push(format!(
                "{}\n",
                serde_json::json!({
                    "outturn": { "pending": [{ "content": steer, "delivery": "steer" }] }
                })
            ));
            ndjson(lines)
        }

        Behaviour::TextThenSteer { first, steer, reply } => {
            let text = if call_number > 1 { reply } else { first };
            let mut lines: Vec<String> = chunk_text(&text)
                .iter()
                .map(|piece| format!("{}\n", chunk_json(piece, None)))
                .collect();
            lines.push(format!("{}\n", chunk_json("", Some("stop"))));
            if call_number == 1 {
                lines.push(format!(
                    "{}\n",
                    serde_json::json!({
                        "outturn": { "pending": [{ "content": steer, "delivery": "steer" }] }
                    })
                ));
            }
            ndjson(lines)
        }

        Behaviour::TruncatedToolCall { name, arguments } => {
            let mut lines = vec![format!(
                "{}\n",
                tool_chunk(Some("call_cut"), Some(&name), &arguments)
            )];
            // finish_reason "length": the model ran out of room mid-call.
            lines.push(format!("{}\n", chunk_json("", Some("length"))));
            ndjson(lines)
        }

        Behaviour::AlwaysToolCall {
            name,
            arguments,
            content,
        } => {
            let mut lines = Vec::new();
            if !content.is_empty() {
                lines.push(format!("{}\n", chunk_json(&content, None)));
            }
            lines.push(format!(
                "{}\n",
                tool_chunk(Some("call_loop"), Some(&name), &arguments)
            ));
            lines.push(format!("{}\n", chunk_json("", Some("tool_calls"))));
            ndjson(lines)
        }

        Behaviour::ToolThenReply {
            name,
            arguments,
            reply,
        } => {
            // The second request is the model answering with the tool result
            // in hand, so it replies with prose and asks for nothing more.
            if call_number > 1 {
                let mut lines: Vec<String> = chunk_text(&reply)
                    .iter()
                    .map(|piece| format!("{}\n", chunk_json(piece, None)))
                    .collect();
                lines.push(format!("{}\n", chunk_json("", Some("stop"))));
                return ndjson(lines);
            }

            let mut lines = vec![format!(
                "{}\n",
                tool_chunk(Some("call_fake_1"), Some(&name), "")
            )];
            // Arguments in pieces: a caller that reads only the first chunk
            // ends up with a fragment of JSON rather than an argument object.
            for piece in arguments.as_bytes().chunks(7) {
                let piece = String::from_utf8_lossy(piece).to_string();
                lines.push(format!("{}\n", tool_chunk(None, None, &piece)));
            }
            lines.push(format!("{}\n", chunk_json("", Some("tool_calls"))));
            ndjson(lines)
        }
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
        Behaviour::ToolThenReply { reply, .. } => reply,
        Behaviour::AlwaysToolCall { content, .. } => content,
        Behaviour::TruncatedToolCall { .. } => String::new(),
        Behaviour::ToolThenSteer { reply, .. } => reply,
        Behaviour::TextThenSteer { reply, .. } => reply,
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
