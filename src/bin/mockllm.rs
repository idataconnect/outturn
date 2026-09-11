//! A model that never was, for load tests that should cost nothing.
//!
//! Speaks all three protocols the gateway speaks -- OpenAI chat-completions,
//! Anthropic messages and Gemini generateContent -- so the gateway dials it the
//! way it dials any provider: through a real socket, with real streaming, real
//! connection reuse and a real read timeout. That is the point of it being a
//! service rather than a branch inside the gateway: a load test against an
//! in-process shortcut measures the shortcut, and the parts most likely to
//! break under load are exactly the ones a shortcut skips.
//!
//! The three differ in where a stream says what it cost, which is the whole
//! reason they are all here rather than only the one. OpenAI says it once, in
//! a chunk *after* the one carrying `finish_reason`; Anthropic says the input
//! side up front and the output side as it goes; Gemini repeats a running
//! total on nearly every chunk. A turn cut short therefore leaves three quite
//! different amounts of truth behind, and code that recovers what it can has
//! to be tried against all three or it is only tried against the easy one.
//!
//! It answers three questions a real provider cannot answer cheaply:
//!
//! Does prompt caching survive the shapes we send? It records the longest
//! common prefix between each request and the last one from the same session,
//! and reports it as `cached_tokens` the way llama.cpp does. A turn that ought
//! to reuse a prefix and does not is then a number rather than a suspicion.
//!
//! What happens when a provider is slow, or rude? Latency, time to first
//! token, and a share of requests that fail or hang are all configurable, so
//! the breaker, the retry path and the idle timeout can be exercised
//! deliberately instead of waited for.
//!
//! Deployed at zero replicas. Scaling it up is the whole of turning it on.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Names what answered, on every response.
///
/// Nothing enforces anything with it. It is there so a transcript served by a
/// fixture can be told apart from one served by a model, months later, by
/// somebody who does not know this service exists.
const MOCK_HEADER: axum::http::HeaderName =
    axum::http::HeaderName::from_static("x-outturn-mock");

/// What that header says. One string, because all three protocols say it.
const MOCK_NOTE: &str = "outturn-mockllm: generated, not inferred";

/// The arguments every mock tool call carries.
///
/// Valid JSON, and the same across protocols, so a test that cuts a stream
/// partway can tell truncated arguments from arguments that were always this
/// shape -- the difference between a bug and a fixture.
const TOOL_ARGUMENTS: &str = r#"{"action":"pretending to work"}"#;

/// Words the filler is built from.
///
/// Deterministic on purpose: a load test that also has to cope with novel text
/// is measuring two things. The words are ordinary enough that a transcript
/// remains readable when someone opens one to see what happened.
const FILLER: &[&str] = &[
    "the", "tide", "moves", "slowly", "across", "flat", "stone", "and", "leaves",
    "small", "pools", "behind", "each", "holds", "a", "little", "world", "that",
    "waits", "for", "water", "to", "return", "again", "before", "dusk",
];

#[derive(Clone)]
struct Config {
    /// Fixed delay before anything is sent.
    ttft: Duration,
    /// How fast tokens are produced once they start.
    tokens_per_sec: f64,
    /// Tokens in a reply, when the request does not ask for tools.
    reply_tokens: usize,
    /// Share of requests answered with 503 rather than a reply.
    drop_rate: f64,
    /// Share of requests that accept the connection and then say nothing,
    /// for exercising the read timeout rather than the error path.
    hang_rate: f64,
    /// Whether a request carrying tools should answer with a tool call.
    tool_calls: bool,
}

impl Config {
    fn from_env() -> Self {
        fn num<T: std::str::FromStr>(name: &str, default: T) -> T {
            std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        }
        Self {
            ttft: Duration::from_millis(num("MOCK_TTFT_MS", 40)),
            tokens_per_sec: num("MOCK_TOKENS_PER_SEC", 400.0),
            reply_tokens: num("MOCK_REPLY_TOKENS", 64),
            drop_rate: num("MOCK_DROP_RATE", 0.0),
            hang_rate: num("MOCK_HANG_RATE", 0.0),
            tool_calls: num::<u8>("MOCK_TOOL_CALLS", 1) != 0,
        }
    }
}

#[derive(Default)]
struct Metrics {
    requests: AtomicU64,
    dropped: AtomicU64,
    hung: AtomicU64,
    tool_calls: AtomicU64,
    prompt_tokens: AtomicU64,
    cached_tokens: AtomicU64,
    completion_tokens: AtomicU64,
    /// Requests that reused nothing at all, which for a continuing
    /// conversation is the interesting failure.
    cold_prompts: AtomicU64,
    /// Streams the client stopped reading before they ended.
    ///
    /// The whole of testing a stop button is whether the provider found out.
    /// A cancel that only hides the tail in a browser leaves this at zero
    /// while everything on screen looks right.
    abandoned: AtomicU64,
    /// Tokens already sent when a stream was abandoned, summed. What a turn
    /// cut short actually cost, against which recovered usage is checked.
    abandoned_tokens: AtomicU64,
}

struct AppState {
    config: Config,
    metrics: Metrics,
    /// The last prompt seen per conversation, for measuring reuse. Keyed by
    /// whatever identifies a conversation in the request; see `session_key`.
    seen: Mutex<HashMap<String, Vec<String>>>,
    /// Turns a share into a decision without pulling in a random number
    /// generator: a counter is enough to make "one in twenty" true over a run,
    /// and it makes a load test reproducible.
    counter: AtomicU64,
}

#[derive(Debug, Deserialize)]
struct ChatRequest {
    #[serde(default)]
    model: String,
    messages: Vec<Message>,
    #[serde(default)]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Message {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// A stand-in for tokenising: four characters to a token is close enough to
/// the real ratio for the numbers to mean something, and it costs nothing.
fn tokens_of(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// One line per message, so a prefix comparison happens at message boundaries.
///
/// That is the granularity that matters here: a provider's cache is broken by
/// a message being reworded, reordered, or split into several -- which is
/// exactly what replaying a turn's tool calls does.
fn prompt_lines(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|m| {
            format!(
                "{}|{}|{}|{}",
                m.role,
                m.content.as_deref().unwrap_or(""),
                m.tool_calls.as_ref().map(|t| t.to_string()).unwrap_or_default(),
                m.tool_call_id.as_deref().unwrap_or("")
            )
        })
        .collect()
}

/// What a conversation is, for the purpose of measuring reuse.
///
/// The first message is almost always the system prompt, which is stable for
/// the life of an agent and distinct between agents. Good enough to tell two
/// concurrent conversations apart in a load test without the caller having to
/// announce itself.
fn session_key(lines: &[String]) -> String {
    lines.first().cloned().unwrap_or_else(|| "empty".into())
}

fn common_prefix(previous: &[String], current: &[String]) -> usize {
    previous
        .iter()
        .zip(current)
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| tokens_of(a))
        .sum()
}

fn filler(tokens: usize) -> String {
    let mut out = String::new();
    for i in 0..tokens {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(FILLER[i % FILLER.len()]);
    }
    out
}

/// True for the given share of calls, spread evenly rather than randomly.
fn every(share: f64, counter: &AtomicU64) -> bool {
    if share <= 0.0 {
        return false;
    }
    if share >= 1.0 {
        return true;
    }
    let n = counter.fetch_add(1, Ordering::Relaxed);
    let period = (1.0 / share).round().max(1.0) as u64;
    n % period == 0
}

/// Notices a stream that was dropped before it finished.
///
/// A stream ends one of two ways: its generator runs to completion, or the
/// client goes away and the body is dropped where it was suspended. Only the
/// second leaves a `Drop` to run with `done` still false, which is what makes
/// this the one place the difference is observable -- and the difference is
/// exactly what a stop button has to be judged on.
struct StreamGuard {
    metrics: Arc<AppState>,
    sent: usize,
    done: bool,
}

impl StreamGuard {
    fn new(state: &Arc<AppState>) -> Self {
        Self { metrics: Arc::clone(state), sent: 0, done: false }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.metrics.metrics.abandoned.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .metrics
            .abandoned_tokens
            .fetch_add(self.sent as u64, Ordering::Relaxed);
    }
}

/// What a request is answered with, before any protocol has been chosen.
///
/// The three encoders below differ only in how they spell this out. Deciding
/// it once means a reply is the same reply whichever wire format asked for it,
/// which is what makes a number measured through one protocol comparable with
/// the same number measured through another.
struct Turn {
    /// Prompt tokens actually evaluated: the whole prompt less what a prefix
    /// match served from cache.
    prompt_tokens: usize,
    /// Of the prompt, how much came from cache. Reported differently by each
    /// protocol -- inside the prompt total or beside it -- which is exactly
    /// the kind of thing that goes unnoticed until a bill disagrees.
    cached_tokens: usize,
    completion_tokens: usize,
    /// Present when this turn answers with a tool call rather than prose.
    tool: Option<String>,
    /// The prose, when there is any.
    text: String,
}

impl Turn {
    /// The reply, one word per streamed chunk. Empty for a tool call.
    fn words(&self) -> Vec<String> {
        if self.tool.is_some() {
            return Vec::new();
        }
        self.text
            .split(' ')
            .enumerate()
            .map(|(i, w)| if i == 0 { w.to_string() } else { format!(" {w}") })
            .collect()
    }
}

/// Decides what to answer, and records what it cost. Protocol-neutral.
///
/// Returns `None` when this request is one of the share configured to fail or
/// hang, having already done the failing or the hanging.
async fn plan(
    state: &AppState,
    messages: &[Message],
    // The tool this turn would call, if the request offered any and the shape
    // of the conversation makes calling one sensible. Named by the caller,
    // which knows how its own protocol spells a tool definition.
    offered_tool: Option<String>,
) -> Option<Turn> {
    let m = &state.metrics;
    m.requests.fetch_add(1, Ordering::Relaxed);

    let lines = prompt_lines(messages);
    let prompt_tokens: usize = lines.iter().map(|l| tokens_of(l)).sum();

    let cached = {
        let key = session_key(&lines);
        let mut seen = state.seen.lock().await;
        let cached = seen.get(&key).map(|prev| common_prefix(prev, &lines)).unwrap_or(0);
        seen.insert(key, lines.clone());
        cached
    };

    // A continuing conversation that reused nothing is what this exists to
    // notice. A first turn legitimately reuses nothing, and is not counted.
    if cached == 0 && messages.len() > 2 {
        m.cold_prompts.fetch_add(1, Ordering::Relaxed);
    }

    let tool = offered_tool.filter(|_| state.config.tool_calls);
    let wants_tool = tool.is_some();
    if wants_tool {
        m.tool_calls.fetch_add(1, Ordering::Relaxed);
    }

    let completion_tokens = if wants_tool { 12 } else { state.config.reply_tokens };
    m.prompt_tokens.fetch_add(prompt_tokens as u64, Ordering::Relaxed);
    m.cached_tokens.fetch_add(cached as u64, Ordering::Relaxed);
    m.completion_tokens.fetch_add(completion_tokens as u64, Ordering::Relaxed);

    tokio::time::sleep(state.config.ttft).await;

    Some(Turn {
        prompt_tokens: prompt_tokens.saturating_sub(cached),
        cached_tokens: cached,
        completion_tokens,
        tool,
        // A tool call answers with no prose, so the filler is not generated.
        text: if wants_tool { String::new() } else { filler(completion_tokens) },
    })
}

/// The failure injections, which happen before anything is planned.
///
/// Separated because all three protocols share them: a breaker that only
/// trips for one wire format has not been tested.
async fn refuse_or_hang(state: &AppState) -> Option<Response> {
    let m = &state.metrics;
    if every(state.config.drop_rate, &state.counter) {
        m.requests.fetch_add(1, Ordering::Relaxed);
        m.dropped.fetch_add(1, Ordering::Relaxed);
        return Some((StatusCode::SERVICE_UNAVAILABLE, "mock: refusing this one").into_response());
    }
    if every(state.config.hang_rate, &state.counter) {
        m.requests.fetch_add(1, Ordering::Relaxed);
        m.hung.fetch_add(1, Ordering::Relaxed);
        // Long enough to outlast any sensible read timeout, and bounded so a
        // load test does not accumulate stuck tasks forever.
        tokio::time::sleep(Duration::from_secs(600)).await;
        return Some((StatusCode::GATEWAY_TIMEOUT, "mock: never mind").into_response());
    }
    None
}

/// The OpenAI chat-completions protocol.
///
/// Usage arrives once, in a chunk *after* the one carrying `finish_reason` --
/// which is the protocol's own shape, not a quirk of this fixture, and the
/// reason a reader that stops at `finish_reason` records nothing for every
/// stream rather than only for interrupted ones.
async fn completions(State(state): State<Arc<AppState>>, Json(req): Json<ChatRequest>) -> Response {
    if let Some(refusal) = refuse_or_hang(&state).await {
        return refusal;
    }

    // Answering a tool result with another tool call is how a loop that never
    // ends begins, so only a turn whose last word came from the user calls one.
    let offered = req
        .tools
        .as_ref()
        .is_some_and(|t| !t.is_empty())
        .then(|| tool_name(&req))
        .filter(|_| req.messages.last().is_some_and(|m| m.role == "user"));

    let Some(turn) = plan(&state, &req.messages, offered).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "mock: nothing to say").into_response();
    };

    // This protocol folds cached tokens into the prompt total, so they are put
    // back before reporting. Anthropic does the opposite; see its encoder.
    let usage = serde_json::json!({
        "prompt_tokens": turn.prompt_tokens + turn.cached_tokens,
        "completion_tokens": turn.completion_tokens,
        "total_tokens": turn.prompt_tokens + turn.cached_tokens + turn.completion_tokens,
        "prompt_tokens_details": { "cached_tokens": turn.cached_tokens },
    });

    if !req.stream {
        let message = match &turn.tool {
            Some(name) => serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "mock_call",
                    "type": "function",
                    "function": { "name": name, "arguments": TOOL_ARGUMENTS },
                }]
            }),
            None => serde_json::json!({ "role": "assistant", "content": turn.text }),
        };
        return ([(MOCK_HEADER, MOCK_NOTE)], Json(serde_json::json!({
            "id": "mock",
            "object": "chat.completion",
            "model": req.model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": if turn.tool.is_some() { "tool_calls" } else { "stop" },
            }],
            "usage": usage,
        })))
        .into_response();
    }

    let per_token = Duration::from_secs_f64(1.0 / state.config.tokens_per_sec.max(1.0));
    let guard_state = Arc::clone(&state);
    let stream = async_stream::stream! {
        let mut guard = StreamGuard::new(&guard_state);
        if let Some(name) = &turn.tool {
            yield Ok::<_, std::io::Error>(sse(&serde_json::json!({
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0, "id": "mock_call", "type": "function",
                    "function": {"name": name, "arguments": TOOL_ARGUMENTS}
                }]}}]
            })));
            yield Ok(sse(&serde_json::json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
            })));
        } else {
            for word in turn.words() {
                tokio::time::sleep(per_token).await;
                guard.sent += 1;
                yield Ok(sse(&serde_json::json!({
                    "choices": [{"index": 0, "delta": {"content": word}}]
                })));
            }
            yield Ok(sse(&serde_json::json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            })));
        }
        // After `finish_reason`, not on it. A stream cut before this point
        // carries no usage at all, which is the case worth being able to test.
        yield Ok(sse(&serde_json::json!({ "choices": [], "usage": usage })));
        yield Ok(axum::body::Bytes::from_static(b"data: [DONE]\n\n"));
        guard.done = true;
    };

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/event-stream"),
            // Said on every response, because the one time it matters is when
            // somebody is looking at a transcript wondering where the words
            // came from. A component that fabricates model output should be
            // identifiable after the fact rather than only by knowing which
            // service answered.
            (MOCK_HEADER, MOCK_NOTE),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

/// A request in Anthropic's `/v1/messages` shape.
///
/// Its messages carry content as either a string or a sequence of blocks, and
/// a turn that has used tools is always the second. Both are flattened to the
/// one line-per-message form the cache accounting works in.
#[derive(Debug, Deserialize)]
struct AnthropicRequest {
    #[serde(default)]
    model: String,
    #[serde(default)]
    system: Option<serde_json::Value>,
    messages: Vec<serde_json::Value>,
    #[serde(default)]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    stream: bool,
}

impl AnthropicRequest {
    /// The messages, in the shared shape. Content blocks are joined rather
    /// than dropped: a prompt whose tool results vanish measures a cache hit
    /// that the real prompt would not get.
    fn flattened(&self) -> Vec<Message> {
        let mut out = Vec::new();
        if let Some(system) = &self.system {
            out.push(Message {
                role: "system".into(),
                content: Some(text_of_content(system)),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        for m in &self.messages {
            out.push(Message {
                role: m["role"].as_str().unwrap_or("user").to_string(),
                content: Some(text_of_content(&m["content"])),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        out
    }
}

/// Anthropic content, as one string: a bare string, or every block's text.
fn text_of_content(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b["type"].as_str() {
                Some("text") => b["text"].as_str().unwrap_or_default().to_string(),
                // A tool result's content is part of the prompt whether or not
                // it is prose, and its size is what the cache accounting needs.
                _ => b["content"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| b.to_string()),
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

/// One Anthropic stream event, in its own framing.
///
/// Anthropic names the event twice -- once in an `event:` line and again in
/// the payload's `type` -- and a reader may use either. Both are sent, because
/// a fixture that sends only the one its own reader happens to use is a
/// fixture that cannot catch the reader being wrong.
fn anthropic_event(kind: &str, value: serde_json::Value) -> axum::body::Bytes {
    let mut payload = value;
    payload["type"] = serde_json::json!(kind);
    axum::body::Bytes::from(format!("event: {kind}\ndata: {payload}\n\n"))
}

/// The Anthropic messages protocol.
///
/// Usage is spread across the stream rather than gathered at its end:
/// `message_start` carries the input side, `message_delta` the output side as
/// it grows. A turn cut short therefore still leaves real numbers behind,
/// which is what makes this protocol the forgiving one to stop.
async fn anthropic_messages(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AnthropicRequest>,
) -> Response {
    if let Some(refusal) = refuse_or_hang(&state).await {
        return refusal;
    }

    let messages = req.flattened();
    let offered = req
        .tools
        .as_ref()
        .is_some_and(|t| !t.is_empty())
        .then(|| {
            req.tools
                .as_ref()
                .and_then(|t| t.first())
                .and_then(|t| t["name"].as_str())
                .unwrap_or("get_current_time")
                .to_string()
        })
        .filter(|_| req.messages.last().is_some_and(|m| m["role"] == "user"));

    let Some(turn) = plan(&state, &messages, offered).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "mock: nothing to say").into_response();
    };

    // Anthropic reports cache tokens *beside* the input total rather than
    // inside it, the opposite of the OpenAI protocol. A reader that adds
    // where it should have subtracted gets a plausible number, which is the
    // worst kind of wrong, so both directions exist here to be tested.
    let input_tokens = turn.prompt_tokens;
    let cache_read = turn.cached_tokens;
    let stop_reason = if turn.tool.is_some() { "tool_use" } else { "end_turn" };

    if !req.stream {
        let content = match &turn.tool {
            Some(name) => serde_json::json!([{
                "type": "tool_use",
                "id": "mock_call",
                "name": name,
                "input": serde_json::from_str::<serde_json::Value>(TOOL_ARGUMENTS).unwrap_or_default(),
            }]),
            None => serde_json::json!([{ "type": "text", "text": turn.text }]),
        };
        return ([(MOCK_HEADER, MOCK_NOTE)], Json(serde_json::json!({
            "id": "msg_mock",
            "type": "message",
            "role": "assistant",
            "model": req.model,
            "content": content,
            "stop_reason": stop_reason,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": turn.completion_tokens,
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": 0,
            },
        })))
        .into_response();
    }

    let per_token = Duration::from_secs_f64(1.0 / state.config.tokens_per_sec.max(1.0));
    let guard_state = Arc::clone(&state);
    let stream = async_stream::stream! {
        let mut guard = StreamGuard::new(&guard_state);
        // The output side is a placeholder here -- a real one reports 1, not
        // 0, and a reader that treats any non-zero value as the final answer
        // bills one token for every stream that stops before `message_delta`.
        yield Ok::<_, std::io::Error>(anthropic_event("message_start", serde_json::json!({
            "message": {
                "id": "msg_mock",
                "type": "message",
                "role": "assistant",
                "model": req.model,
                "content": [],
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": 1,
                    "cache_read_input_tokens": cache_read,
                    "cache_creation_input_tokens": 0,
                },
            }
        })));

        let mut produced = 0usize;
        match &turn.tool {
            Some(name) => {
                yield Ok(anthropic_event("content_block_start", serde_json::json!({
                    "index": 0,
                    "content_block": { "type": "tool_use", "id": "mock_call", "name": name, "input": {} }
                })));
                // Split so a stream cut partway leaves invalid JSON behind,
                // which is what a real one does and what a reader has to cope
                // with rather than assume away.
                let (head, tail) = TOOL_ARGUMENTS.split_at(TOOL_ARGUMENTS.len() / 2);
                for fragment in [head, tail] {
                    tokio::time::sleep(per_token).await;
                    yield Ok(anthropic_event("content_block_delta", serde_json::json!({
                        "index": 0,
                        "delta": { "type": "input_json_delta", "partial_json": fragment }
                    })));
                }
                produced = turn.completion_tokens;
                yield Ok(anthropic_event("content_block_stop", serde_json::json!({ "index": 0 })));
            }
            None => {
                yield Ok(anthropic_event("content_block_start", serde_json::json!({
                    "index": 0,
                    "content_block": { "type": "text", "text": "" }
                })));
                for word in turn.words() {
                    tokio::time::sleep(per_token).await;
                    produced += 1;
                    guard.sent += 1;
                    yield Ok(anthropic_event("content_block_delta", serde_json::json!({
                        "index": 0,
                        "delta": { "type": "text_delta", "text": word }
                    })));
                }
                yield Ok(anthropic_event("content_block_stop", serde_json::json!({ "index": 0 })));
            }
        }

        // Usage at the event's top level, while `stop_reason` sits under
        // `delta`. The split is the protocol's, and a reader that looks for
        // usage under `delta` finds nothing and reports nothing.
        yield Ok(anthropic_event("message_delta", serde_json::json!({
            "delta": { "stop_reason": stop_reason, "stop_sequence": null },
            "usage": { "output_tokens": produced.max(1) },
        })));
        yield Ok(anthropic_event("message_stop", serde_json::json!({})));
        guard.done = true;
    };

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/event-stream"),
            (MOCK_HEADER, MOCK_NOTE),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

/// A request in Gemini's `generateContent` shape.
#[derive(Debug, Deserialize)]
struct GeminiRequest {
    #[serde(default)]
    contents: Vec<serde_json::Value>,
    #[serde(rename = "systemInstruction", default)]
    system_instruction: Option<serde_json::Value>,
    #[serde(default)]
    tools: Option<Vec<serde_json::Value>>,
}

impl GeminiRequest {
    fn flattened(&self) -> Vec<Message> {
        let mut out = Vec::new();
        if let Some(system) = &self.system_instruction {
            out.push(Message {
                role: "system".into(),
                content: Some(gemini_parts_text(&system["parts"])),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        for c in &self.contents {
            out.push(Message {
                // Gemini says "model" where the others say "assistant".
                role: match c["role"].as_str() {
                    Some("model") => "assistant".into(),
                    other => other.unwrap_or("user").to_string(),
                },
                content: Some(gemini_parts_text(&c["parts"])),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        out
    }

    /// The first declared function, if any were declared.
    fn first_tool(&self) -> Option<String> {
        self.tools
            .as_ref()?
            .iter()
            .find_map(|t| t["functionDeclarations"][0]["name"].as_str())
            .map(str::to_string)
    }
}

fn gemini_parts_text(parts: &serde_json::Value) -> String {
    parts
        .as_array()
        .map(|ps| {
            ps.iter()
                .map(|p| match p["text"].as_str() {
                    Some(t) => t.to_string(),
                    None => p.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Gemini's usage object, which counts differently again.
///
/// `cachedContentTokenCount` is *included* in `promptTokenCount`, where
/// Anthropic reports its cache tokens beside the input total. Same idea,
/// opposite arithmetic; a translation that adds where it should subtract
/// inflates every cached conversation.
fn gemini_usage(turn: &Turn, produced: usize) -> serde_json::Value {
    let prompt = turn.prompt_tokens + turn.cached_tokens;
    serde_json::json!({
        "promptTokenCount": prompt,
        "candidatesTokenCount": produced,
        "cachedContentTokenCount": turn.cached_tokens,
        "totalTokenCount": prompt + produced,
    })
}

/// The Gemini generateContent protocol.
///
/// Usage rides nearly every chunk as a running total, so an interrupted stream
/// leaves the most behind of the three. Streaming responses are a JSON array
/// of objects rather than SSE when asked for without `alt=sse`; this serves
/// the SSE form, which is what a streaming client asks for.
async fn gemini_generate(
    State(state): State<Arc<AppState>>,
    // Gemini puts the method on the model segment after a colon, as in
    // `gemini-2.0-flash:streamGenerateContent`, so the two arrive together.
    axum::extract::Path(model_and_method): axum::extract::Path<String>,
    Json(req): Json<GeminiRequest>,
) -> Response {
    let method = model_and_method.rsplit(':').next().unwrap_or_default().to_string();
    if let Some(refusal) = refuse_or_hang(&state).await {
        return refusal;
    }

    let messages = req.flattened();
    let offered = req
        .first_tool()
        .filter(|_| req.contents.last().is_some_and(|c| c["role"] != "model"));

    let Some(turn) = plan(&state, &messages, offered).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "mock: nothing to say").into_response();
    };

    let part = match &turn.tool {
        Some(name) => serde_json::json!({
            "functionCall": {
                "name": name,
                "args": serde_json::from_str::<serde_json::Value>(TOOL_ARGUMENTS).unwrap_or_default(),
            }
        }),
        None => serde_json::json!({ "text": turn.text }),
    };
    // Gemini reports STOP for a function call as well as for prose: the call
    // is the reply, not an interruption of one.
    let finish = "STOP";

    if !method.starts_with("streamGenerateContent") {
        return ([(MOCK_HEADER, MOCK_NOTE)], Json(serde_json::json!({
            "candidates": [{
                "content": { "role": "model", "parts": [part] },
                "finishReason": finish,
                "index": 0,
            }],
            "usageMetadata": gemini_usage(&turn, turn.completion_tokens),
        })))
        .into_response();
    }

    let per_token = Duration::from_secs_f64(1.0 / state.config.tokens_per_sec.max(1.0));
    let guard_state = Arc::clone(&state);
    let stream = async_stream::stream! {
        let mut guard = StreamGuard::new(&guard_state);
        let line = |v: serde_json::Value| axum::body::Bytes::from(format!("data: {v}\n\n"));
        match &turn.tool {
            Some(_) => {
                yield Ok::<_, std::io::Error>(line(serde_json::json!({
                    "candidates": [{
                        "content": { "role": "model", "parts": [part] },
                        "finishReason": finish,
                        "index": 0,
                    }],
                    "usageMetadata": gemini_usage(&turn, turn.completion_tokens),
                })));
            }
            None => {
                let words = turn.words();
                let last = words.len().saturating_sub(1);
                for (i, word) in words.into_iter().enumerate() {
                    tokio::time::sleep(per_token).await;
                    guard.sent += 1;
                    // The running total on every chunk: what makes a stopped
                    // Gemini turn still able to say what it cost.
                    yield Ok(line(serde_json::json!({
                        "candidates": [{
                            "content": { "role": "model", "parts": [{ "text": word }] },
                            "index": 0,
                            "finishReason": if i == last { Some(finish) } else { None },
                        }],
                        "usageMetadata": gemini_usage(&turn, i + 1),
                    })));
                }
            }
        }
    };

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/event-stream"),
            (MOCK_HEADER, MOCK_NOTE),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

fn tool_name(req: &ChatRequest) -> String {
    req.tools
        .as_ref()
        .and_then(|t| t.first())
        .and_then(|t| t["function"]["name"].as_str())
        .unwrap_or("get_current_time")
        .to_string()
}

/// Wraps a chunk's variable part in the envelope every chunk carries.
///
/// `id`, `object`, `created` and `model` are not decoration: a consumer
/// deserialises the whole chunk, so one missing field drops the chunk entirely
/// and the reply arrives empty with only a warning in a log. A fixture that
/// omits them tests the consumer's error path rather than its success path,
/// convincingly enough to look like a platform bug.
fn sse(value: &serde_json::Value) -> axum::body::Bytes {
    let mut chunk = serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "mock",
    });
    if let Some(fields) = value.as_object() {
        for (k, v) in fields {
            chunk[k] = v.clone();
        }
    }
    axum::body::Bytes::from(format!("data: {chunk}\n\n"))
}

#[derive(Serialize)]
struct Report {
    requests: u64,
    dropped: u64,
    hung: u64,
    tool_calls: u64,
    prompt_tokens: u64,
    cached_tokens: u64,
    completion_tokens: u64,
    /// Continuing conversations that reused nothing.
    cold_prompts: u64,
    /// Streams the client stopped reading, and what had been sent by then.
    abandoned: u64,
    abandoned_tokens: u64,
    /// Cached over total prompt tokens. The number this service exists for.
    cache_hit_rate: f64,
}

async fn metrics(State(state): State<Arc<AppState>>) -> Json<Report> {
    let m = &state.metrics;
    let prompt = m.prompt_tokens.load(Ordering::Relaxed);
    let cached = m.cached_tokens.load(Ordering::Relaxed);
    Json(Report {
        requests: m.requests.load(Ordering::Relaxed),
        dropped: m.dropped.load(Ordering::Relaxed),
        hung: m.hung.load(Ordering::Relaxed),
        tool_calls: m.tool_calls.load(Ordering::Relaxed),
        prompt_tokens: prompt,
        cached_tokens: cached,
        completion_tokens: m.completion_tokens.load(Ordering::Relaxed),
        cold_prompts: m.cold_prompts.load(Ordering::Relaxed),
        abandoned: m.abandoned.load(Ordering::Relaxed),
        abandoned_tokens: m.abandoned_tokens.load(Ordering::Relaxed),
        cache_hit_rate: if prompt == 0 { 0.0 } else { cached as f64 / prompt as f64 },
    })
}

async fn reset(State(state): State<Arc<AppState>>) -> StatusCode {
    let m = &state.metrics;
    for c in [
        &m.requests, &m.dropped, &m.hung, &m.tool_calls, &m.prompt_tokens,
        &m.cached_tokens, &m.completion_tokens, &m.cold_prompts,
        &m.abandoned, &m.abandoned_tokens,
    ] {
        c.store(0, Ordering::Relaxed);
    }
    state.seen.lock().await.clear();
    StatusCode::NO_CONTENT
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let config = Config::from_env();
    tracing::info!(
        ttft_ms = config.ttft.as_millis() as u64,
        tokens_per_sec = config.tokens_per_sec,
        drop_rate = config.drop_rate,
        hang_rate = config.hang_rate,
        "mock model starting"
    );

    let state = Arc::new(AppState {
        config,
        metrics: Metrics::default(),
        seen: Mutex::new(HashMap::new()),
        counter: AtomicU64::new(0),
    });

    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        // Anthropic and Gemini at the paths their own clients use, so a route
        // is configured here exactly as it would be against the real thing.
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1beta/models/{model}", post(gemini_generate))
        .route("/metrics", get(metrics))
        .route("/reset", post(reset))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(|| async { "ok" }))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8083").await.expect("bind");
    tracing::info!("listening on 0.0.0.0:8083");
    axum::serve(listener, app).await.expect("serve");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn tool_call(id: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: Some(String::new()),
            tool_calls: Some(serde_json::json!([{ "id": id, "function": { "name": "f" } }])),
            tool_call_id: None,
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(id.into()),
        }
    }

    #[test]
    fn a_continuing_conversation_reuses_everything_before_the_new_message() {
        let first = prompt_lines(&[msg("system", "be helpful"), msg("user", "hello")]);
        let second = prompt_lines(&[
            msg("system", "be helpful"),
            msg("user", "hello"),
            msg("assistant", "hi"),
            msg("user", "more"),
        ]);
        let reused = common_prefix(&first, &second);
        let whole_of_first: usize = first.iter().map(|l| tokens_of(l)).sum();
        assert_eq!(
            reused, whole_of_first,
            "a conversation that only grew reused less than all of what it had"
        );
    }

    /// A turn's tool calls are replayed on the next turn, and a stored
    /// assistant message becomes three. The question this whole service exists
    /// to answer is whether that rewrites history or only appends to it.
    #[test]
    fn replaying_a_tool_turn_does_not_disturb_what_came_before() {
        let before = prompt_lines(&[
            msg("system", "be helpful"),
            msg("user", "hello"),
            msg("assistant", "hi"),
            msg("user", "what is the weather"),
        ]);
        let after = prompt_lines(&[
            msg("system", "be helpful"),
            msg("user", "hello"),
            msg("assistant", "hi"),
            msg("user", "what is the weather"),
            // The three the projection expands one stored message into.
            tool_call("c1"),
            tool_result("c1", "{\"temp\":74}"),
            msg("assistant", "It is 74 degrees."),
            msg("user", "thanks"),
        ]);

        let reused = common_prefix(&before, &after);
        let whole_of_before: usize = before.iter().map(|l| tokens_of(l)).sum();
        assert_eq!(
            reused, whole_of_before,
            "replaying a tool turn changed an earlier message, so every prompt \
             after a tool call is evaluated from that point rather than reused"
        );
    }

    #[test]
    fn rewording_an_earlier_turn_is_noticed() {
        // The failure the test above is guarding against, so that test cannot
        // pass by measuring nothing.
        let before = prompt_lines(&[msg("system", "be helpful"), msg("user", "hello"), msg("assistant", "hi")]);
        let after = prompt_lines(&[
            msg("system", "be helpful"),
            msg("user", "hello"),
            msg("assistant", "hello there"),
            msg("user", "more"),
        ]);
        let reused = common_prefix(&before, &after);
        let whole_of_before: usize = before.iter().map(|l| tokens_of(l)).sum();
        assert!(
            reused < whole_of_before,
            "a reworded message went unnoticed, so this measures nothing"
        );
    }

    /// A chunk missing a field the consumer requires is dropped whole, and a
    /// reply arrives empty with nothing but a warning in a log to say why.
    /// That is what this fixture did, and it looked exactly like a platform
    /// bug: a hundred and fifty turns that succeeded and produced nothing.
    #[test]
    fn every_streamed_chunk_carries_the_whole_envelope() {
        let bytes = sse(&serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": "the"}}]
        }));
        let text = String::from_utf8(bytes.to_vec()).expect("utf-8");
        let json = text
            .strip_prefix("data: ")
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t.trim()).ok())
            .expect("a chunk should be json behind a data: prefix");

        for field in ["id", "object", "created", "model", "choices"] {
            assert!(
                json.get(field).is_some(),
                "a chunk without {field} is discarded by the consumer, and the \
                 reply comes back empty"
            );
        }
        assert_eq!(json["choices"][0]["delta"]["content"], "the");
    }

    /// The three protocols say what a turn cost in three different places,
    /// and a reader that handles one is not thereby a reader that handles the
    /// others. These pin down where each one puts it.
    #[test]
    fn anthropic_reports_usage_at_the_event_top_level() {
        let bytes = anthropic_event(
            "message_delta",
            serde_json::json!({
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 7 },
            }),
        );
        let text = String::from_utf8(bytes.to_vec()).expect("utf-8");
        let json: serde_json::Value = text
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .and_then(|t| serde_json::from_str(t).ok())
            .expect("a data: line of json");

        assert_eq!(json["usage"]["output_tokens"], 7, "usage belongs at the top level");
        assert!(
            json["delta"]["usage"].is_null(),
            "usage under delta is where a reader looks and finds nothing"
        );
        assert_eq!(json["delta"]["stop_reason"], "end_turn", "but stop_reason is under delta");
        assert!(text.starts_with("event: message_delta\n"), "the event is named twice");
    }

    /// The two protocols count cache tokens in opposite directions, which is
    /// the arithmetic most likely to be got wrong in a way that still looks
    /// plausible.
    #[test]
    fn cached_tokens_sit_inside_one_prompt_total_and_beside_the_other() {
        let turn = Turn {
            prompt_tokens: 80,
            cached_tokens: 20,
            completion_tokens: 5,
            tool: None,
            text: String::new(),
        };

        // Gemini: cached is part of the prompt count, so the two must not be
        // added or the prompt is counted twice.
        let gemini = gemini_usage(&turn, 5);
        assert_eq!(gemini["promptTokenCount"], 100);
        assert_eq!(gemini["cachedContentTokenCount"], 20);
        assert_eq!(
            gemini["promptTokenCount"].as_u64().unwrap()
                - gemini["cachedContentTokenCount"].as_u64().unwrap(),
            80,
            "subtracting is what recovers what was actually evaluated"
        );

        // Anthropic: cached sits beside the input count, so the two are added
        // to get the size of the prompt.
        let input = turn.prompt_tokens;
        let cache_read = turn.cached_tokens;
        assert_eq!(input + cache_read, 100, "adding is what recovers the whole prompt");
    }

    /// Everything above is a unit on a shape. This serves the three protocols
    /// over a real socket and reads them back the way the gateway will, so
    /// what is asserted is what a client actually receives.
    async fn serve() -> (String, Arc<AppState>) {
        let state = Arc::new(AppState {
            config: Config {
                ttft: Duration::ZERO,
                tokens_per_sec: 100_000.0,
                reply_tokens: 8,
                drop_rate: 0.0,
                hang_rate: 0.0,
                tool_calls: true,
            },
            metrics: Metrics::default(),
            seen: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        });
        let app = Router::new()
            .route("/v1/chat/completions", post(completions))
            .route("/v1/messages", post(anthropic_messages))
            .route("/v1beta/models/{model}", post(gemini_generate))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        (format!("http://{addr}"), state)
    }

    /// Every `data:` payload of a streamed response, in order.
    fn payloads(body: &str) -> Vec<serde_json::Value> {
        body.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|t| *t != "[DONE]")
            .filter_map(|t| serde_json::from_str(t).ok())
            .collect()
    }

    /// The OpenAI protocol puts usage after `finish_reason`, so a reader that
    /// stops at the finish records nothing -- for every stream, not only an
    /// interrupted one.
    #[tokio::test]
    async fn openai_sends_usage_after_the_finish_reason() {
        let (base, _state) = serve().await;
        let body = reqwest::Client::new()
            .post(format!("{base}/v1/chat/completions"))
            .json(&serde_json::json!({
                "model": "mock",
                "stream": true,
                "messages": [{"role": "user", "content": "hello"}],
            }))
            .send()
            .await
            .expect("send")
            .text()
            .await
            .expect("body");

        let chunks = payloads(&body);
        let finish = chunks
            .iter()
            .position(|c| c["choices"][0]["finish_reason"].is_string())
            .expect("a chunk carrying finish_reason");
        let usage = chunks
            .iter()
            .position(|c| c.get("usage").is_some_and(|u| !u.is_null()))
            .expect("a chunk carrying usage");

        assert!(
            usage > finish,
            "usage arrived at or before the finish, so this fixture cannot \
             show the case where stopping at the finish loses it"
        );
        assert!(
            chunks[usage]["choices"].as_array().is_some_and(|c| c.is_empty()),
            "the usage chunk carries no choices"
        );
    }

    /// Anthropic says the input side before any text and the output side as it
    /// goes, which is what leaves real numbers behind when a turn is stopped.
    #[tokio::test]
    async fn anthropic_reports_input_up_front_and_output_as_it_goes() {
        let (base, _state) = serve().await;
        let body = reqwest::Client::new()
            .post(format!("{base}/v1/messages"))
            .json(&serde_json::json!({
                "model": "mock",
                "stream": true,
                "messages": [{"role": "user", "content": "hello"}],
            }))
            .send()
            .await
            .expect("send")
            .text()
            .await
            .expect("body");

        let events = payloads(&body);
        let start = events.iter().find(|e| e["type"] == "message_start").expect("message_start");
        assert!(
            start["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0) > 0,
            "the input side is known before a single token of output"
        );
        assert_eq!(
            start["message"]["usage"]["output_tokens"], 1,
            "a placeholder, and truthy: a reader taking any non-zero value as \
             final bills one token for every stream stopped before the delta"
        );

        let delta = events.iter().find(|e| e["type"] == "message_delta").expect("message_delta");
        assert!(
            delta["usage"]["output_tokens"].as_u64().unwrap_or(0) > 1,
            "the real output count arrives at the top level of message_delta"
        );
        assert!(delta["delta"]["usage"].is_null(), "and not underneath delta");
    }

    /// Gemini repeats a running total, so an interrupted stream has usage on
    /// whatever chunk it got to.
    #[tokio::test]
    async fn gemini_repeats_a_running_total_on_every_chunk() {
        let (base, _state) = serve().await;
        let body = reqwest::Client::new()
            .post(format!("{base}/v1beta/models/mock:streamGenerateContent"))
            .json(&serde_json::json!({
                "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
            }))
            .send()
            .await
            .expect("send")
            .text()
            .await
            .expect("body");

        let chunks = payloads(&body);
        assert!(chunks.len() > 1, "a stream of one chunk proves nothing about repetition");
        assert!(
            chunks.iter().all(|c| c["usageMetadata"]["totalTokenCount"].as_u64().is_some()),
            "every chunk carries the running total, which is what survives a stop"
        );

        let counts: Vec<u64> = chunks
            .iter()
            .filter_map(|c| c["usageMetadata"]["candidatesTokenCount"].as_u64())
            .collect();
        assert!(
            counts.windows(2).all(|w| w[1] >= w[0]),
            "the running total only grows: {counts:?}"
        );
    }

    /// The whole of testing a stop button: did the provider find out?
    ///
    /// Both halves matter. A stream read to its end must *not* be recorded as
    /// abandoned, or the count means nothing; one dropped partway must be.
    #[tokio::test]
    async fn a_stream_dropped_partway_is_recorded_as_abandoned() {
        use futures::StreamExt;

        let (base, state) = serve().await;
        let ask = |base: String| async move {
            reqwest::Client::new()
                .post(format!("{base}/v1/chat/completions"))
                .json(&serde_json::json!({
                    "model": "mock",
                    "stream": true,
                    "messages": [{"role": "user", "content": "hello"}],
                }))
                .send()
                .await
                .expect("send")
        };

        // Read to the end: the server should see a stream that finished.
        let whole = ask(base.clone()).await.text().await.expect("body");
        assert!(whole.contains("[DONE]"), "the first stream should run to its end");
        assert_eq!(
            state.metrics.abandoned.load(Ordering::Relaxed),
            0,
            "a stream read to its end was counted as abandoned, so the count \
             cannot tell a stop from an ordinary finish"
        );

        // Take one chunk and drop the rest, which is what a cancel does.
        let mut body = ask(base).await.bytes_stream();
        let _first = body.next().await;
        drop(body);

        // The drop travels over a socket, so it is not instant.
        for _ in 0..200 {
            if state.metrics.abandoned.load(Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            state.metrics.abandoned.load(Ordering::Relaxed),
            1,
            "the provider never learned the client had gone, so a stop built \
             on this would stop nothing upstream"
        );
    }

    #[test]
    fn a_share_becomes_that_share_of_calls() {
        let counter = AtomicU64::new(0);
        let hits = (0..100).filter(|_| every(0.1, &counter)).count();
        assert_eq!(hits, 10, "one in ten was not one in ten");
        assert!(!every(0.0, &counter), "zero should never fire");
        assert!(every(1.0, &counter), "one should always fire");
    }
}
