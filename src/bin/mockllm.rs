//! A model that never was, for load tests that should cost nothing.
//!
//! Speaks the OpenAI chat-completions protocol, so the gateway dials it the way
//! it dials any provider -- through a real socket, with real streaming, real
//! connection reuse and a real read timeout. That is the point of it being a
//! service rather than a branch inside the gateway: a load test against an
//! in-process shortcut measures the shortcut, and the parts most likely to
//! break under load are exactly the ones a shortcut skips.
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

async fn completions(State(state): State<Arc<AppState>>, Json(req): Json<ChatRequest>) -> Response {
    let m = &state.metrics;
    m.requests.fetch_add(1, Ordering::Relaxed);

    if every(state.config.drop_rate, &state.counter) {
        m.dropped.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::SERVICE_UNAVAILABLE, "mock: refusing this one").into_response();
    }

    if every(state.config.hang_rate, &state.counter) {
        m.hung.fetch_add(1, Ordering::Relaxed);
        // Long enough to outlast any sensible read timeout, and bounded so a
        // load test does not accumulate stuck tasks forever.
        tokio::time::sleep(Duration::from_secs(600)).await;
        return (StatusCode::GATEWAY_TIMEOUT, "mock: never mind").into_response();
    }

    let lines = prompt_lines(&req.messages);
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
    if cached == 0 && req.messages.len() > 2 {
        m.cold_prompts.fetch_add(1, Ordering::Relaxed);
    }

    let wants_tool = state.config.tool_calls
        && req.tools.as_ref().is_some_and(|t| !t.is_empty())
        // Only when the last message is from the user: answering a tool result
        // with another tool call is how a loop that never ends begins.
        && req.messages.last().is_some_and(|m| m.role == "user");

    if wants_tool {
        m.tool_calls.fetch_add(1, Ordering::Relaxed);
    }

    let body_tokens = if wants_tool { 12 } else { state.config.reply_tokens };
    m.prompt_tokens.fetch_add(prompt_tokens as u64, Ordering::Relaxed);
    m.cached_tokens.fetch_add(cached as u64, Ordering::Relaxed);
    m.completion_tokens.fetch_add(body_tokens as u64, Ordering::Relaxed);

    let usage = serde_json::json!({
        // Reported the way llama.cpp does: what was actually evaluated, with
        // what came from cache accounted separately. A caller that adds the
        // two gets the size of the prompt.
        "prompt_tokens": prompt_tokens.saturating_sub(cached),
        "completion_tokens": body_tokens,
        "total_tokens": prompt_tokens.saturating_sub(cached) + body_tokens,
        "prompt_tokens_details": { "cached_tokens": cached },
    });

    tokio::time::sleep(state.config.ttft).await;

    if !req.stream {
        let message = if wants_tool {
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "mock_call",
                    "type": "function",
                    "function": {
                        "name": tool_name(&req),
                        "arguments": r#"{"action":"pretending to work"}"#,
                    }
                }]
            })
        } else {
            serde_json::json!({ "role": "assistant", "content": filler(body_tokens) })
        };
        return ([(MOCK_HEADER, "outturn-mockllm: generated, not inferred")], Json(serde_json::json!({
            "id": "mock",
            "object": "chat.completion",
            "model": req.model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": if wants_tool { "tool_calls" } else { "stop" },
            }],
            "usage": usage,
        })))
        .into_response();
    }

    let per_token = Duration::from_secs_f64(1.0 / state.config.tokens_per_sec.max(1.0));
    let name = tool_name(&req);
    let stream = async_stream::stream! {
        if wants_tool {
            yield Ok::<_, std::io::Error>(sse(&serde_json::json!({
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0, "id": "mock_call", "type": "function",
                    "function": {"name": name, "arguments": r#"{"action":"pretending to work"}"#}
                }]}}]
            })));
            yield Ok(sse(&serde_json::json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
            })));
        } else {
            for (i, word) in filler(body_tokens).split(' ').enumerate() {
                tokio::time::sleep(per_token).await;
                let text = if i == 0 { word.to_string() } else { format!(" {word}") };
                yield Ok(sse(&serde_json::json!({
                    "choices": [{"index": 0, "delta": {"content": text}}]
                })));
            }
            yield Ok(sse(&serde_json::json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            })));
        }
        // Usage last, as a provider does when asked for it.
        yield Ok(sse(&serde_json::json!({ "choices": [], "usage": usage })));
        yield Ok(axum::body::Bytes::from_static(b"data: [DONE]\n\n"));
    };

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/event-stream"),
            // Said on every response, because the one time it matters is when
            // somebody is looking at a transcript wondering where the words
            // came from. A component that fabricates model output should be
            // identifiable after the fact rather than only by knowing which
            // service answered.
            (MOCK_HEADER, "outturn-mockllm: generated, not inferred"),
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

fn sse(value: &serde_json::Value) -> axum::body::Bytes {
    axum::body::Bytes::from(format!("data: {value}\n\n"))
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
        cache_hit_rate: if prompt == 0 { 0.0 } else { cached as f64 / prompt as f64 },
    })
}

async fn reset(State(state): State<Arc<AppState>>) -> StatusCode {
    let m = &state.metrics;
    for c in [
        &m.requests, &m.dropped, &m.hung, &m.tool_calls, &m.prompt_tokens,
        &m.cached_tokens, &m.completion_tokens, &m.cold_prompts,
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

    #[test]
    fn a_share_becomes_that_share_of_calls() {
        let counter = AtomicU64::new(0);
        let hits = (0..100).filter(|_| every(0.1, &counter)).count();
        assert_eq!(hits, 10, "one in ten was not one in ten");
        assert!(!every(0.0, &counter), "zero should never fire");
        assert!(every(1.0, &counter), "one should always fire");
    }
}
