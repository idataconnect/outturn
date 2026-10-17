//! Each provider, driven against the mock model over a real socket.
//!
//! The unit tests beside each translator feed it events by hand, which proves
//! the translation but not that anything produces those events. These start
//! the fixture, dial it the way the gateway dials a provider, and read what
//! comes back -- so a translator that is right about a shape nobody sends
//! fails here rather than in production.
//!
//! The mock is the one in `src/bin/mockllm.rs`, started in-process. It speaks
//! all three protocols; these tests are what keep its spelling and each
//! translator's expectations from drifting apart.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use outturn::gateway::llm::provider::LlmProvider;
use outturn::gateway::llm::provider::anthropic::AnthropicProvider;
use outturn::gateway::llm::provider::gemini::GeminiProvider;
use outturn::llm::{ChatCompletionRequest, Message, MessageContent, Role};

/// The mock, running for as long as the test needs it.
struct Mock {
    child: Child,
    base_url: String,
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the mock on a free port and waits for it to answer.
async fn start_mock() -> Mock {
    // Port 0 would be ideal, but the fixture binds a fixed one; a port chosen
    // here and handed over keeps concurrent tests from colliding.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    };

    let child = Command::new(env!("CARGO_BIN_EXE_mockllm"))
        .env("MOCK_PORT", port.to_string())
        .env("MOCK_TTFT_MS", "0")
        .env("MOCK_TOKENS_PER_SEC", "10000")
        .env("MOCK_REPLY_TOKENS", "12")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the mock should start");

    // Wrapped before the wait rather than after it. `Child` does not kill on
    // drop, so the timeout below used to panic while the process it spawned
    // went on holding the port -- a stray mockllm per failed run, in exactly
    // the situation where something is already wrong. Owned by `Mock` from
    // here on, its Drop runs on the panic path too.
    let mock = Mock {
        child,
        base_url: format!("http://127.0.0.1:{port}"),
    };
    // Until it answers, with no deadline: a mock that never starts hangs here,
    // where five seconds failed a machine busy compiling. The pause only keeps
    // the loop from spinning.
    while reqwest::get(format!("{}/healthz", mock.base_url))
        .await
        .is_err()
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    mock
}

fn ask(text: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "mock".into(),
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text(text.into()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: Vec::new(),
            signed_by: None,
        }],
        temperature: None,
        max_tokens: None,
        tools: None,
        reasoning_effort: None,
        stream: false,
        stream_options: None,
    }
}

/// Anthropic's events, through the real translator, off a real socket.
///
/// The text has to arrive in pieces and the usage has to survive the trip:
/// the input count is named once at the start and the output count only at
/// the end, and a reader that kept just the latest would have lost one.
#[tokio::test]
async fn anthropic_streams_text_and_keeps_both_halves_of_its_usage() {
    use futures::StreamExt;

    let mock = start_mock().await;
    let provider = AnthropicProvider::new(mock.base_url.clone(), "test-key".into());

    let mut stream = provider
        .chat_completion_stream(&ask("hello"))
        .await
        .expect("the mock should accept a streaming request");

    let mut text = String::new();
    let mut chunks = 0usize;
    let mut last_usage = None;
    let mut finish = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("no chunk should be an error");
        chunks += 1;
        if let Some(content) = &chunk.choices[0].delta.content {
            text.push_str(content);
        }
        if let Some(reason) = &chunk.choices[0].finish_reason {
            finish = Some(reason.clone());
        }
        if let Some(usage) = chunk.usage {
            last_usage = Some(usage);
        }
    }

    assert!(
        chunks > 2,
        "a reply that arrived in one chunk did not stream"
    );
    assert!(!text.is_empty(), "the reply had no text in it");
    assert_eq!(finish.as_deref(), Some("stop"));

    let usage = last_usage.expect("the stream should have reported usage");
    assert!(
        usage.prompt_tokens > 0,
        "the input count was named in message_start and then lost"
    );
    assert!(
        usage.completion_tokens > 1,
        "the output count is still the placeholder from message_start, so a \
         turn stopped early would bill one token"
    );
}

/// A tool call, whose arguments arrive as fragments to be assembled.
#[tokio::test]
async fn anthropic_streams_a_tool_call_in_fragments() {
    use futures::StreamExt;

    let mock = start_mock().await;
    let provider = AnthropicProvider::new(mock.base_url.clone(), "test-key".into());

    let mut request = ask("what is the time");
    request.tools = Some(vec![
        serde_json::from_value(serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_current_time",
                "description": "the time",
                "parameters": { "type": "object", "properties": {} }
            }
        }))
        .expect("a tool definition"),
    ]);

    let mut stream = provider
        .chat_completion_stream(&request)
        .await
        .expect("streaming request");

    let mut name = None;
    let mut arguments = String::new();
    let mut finish = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("no chunk should be an error");
        if let Some(reason) = &chunk.choices[0].finish_reason {
            finish = Some(reason.clone());
        }
        for call in chunk.choices[0].delta.tool_calls.iter().flatten() {
            if let Some(function) = &call.function {
                if let Some(n) = &function.name
                    && !n.is_empty()
                {
                    name = Some(n.clone());
                }
                if let Some(args) = &function.arguments {
                    arguments.push_str(args);
                }
            }
        }
    }

    assert_eq!(name.as_deref(), Some("get_current_time"));
    assert_eq!(
        finish.as_deref(),
        Some("tool_calls"),
        "a tool call is not an ordinary stop"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&arguments).is_ok(),
        "the fragments did not reassemble into valid json: {arguments:?}"
    );
}

/// Gemini repeats a running total, so a turn interrupted anywhere still knows
/// most of what it cost -- there is no single final event holding the only
/// copy, which is what makes this the opposite case to the OpenAI protocol.
#[tokio::test]
async fn gemini_streams_text_and_carries_usage_on_the_way() {
    use futures::StreamExt;

    let mock = start_mock().await;
    let provider = GeminiProvider::new(mock.base_url.clone(), "test-key".into(), "mock".into());

    let mut stream = provider
        .chat_completion_stream(&ask("hello"))
        .await
        .expect("the mock should accept a streaming request");

    let mut text = String::new();
    let mut chunks_with_usage = 0usize;
    let mut chunks = 0usize;
    let mut last_prompt_tokens = 0;
    let mut last_cached = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("no chunk should be an error");
        chunks += 1;
        if let Some(content) = &chunk.choices[0].delta.content {
            text.push_str(content);
        }
        if let Some(usage) = chunk.usage {
            chunks_with_usage += 1;
            last_prompt_tokens = usage.prompt_tokens;
            last_cached = usage
                .prompt_tokens_details
                .as_ref()
                .map(|d| d.cached_tokens)
                .unwrap_or(0);
        }
    }

    assert!(
        chunks > 2,
        "a reply that arrived in one chunk did not stream"
    );
    assert!(!text.is_empty(), "the reply had no text in it");
    assert!(
        chunks_with_usage > 1,
        "usage rode only one chunk, so a stream cut anywhere else would carry \
         none -- which is the case this protocol is supposed to avoid"
    );
    assert!(
        last_prompt_tokens + last_cached > 0,
        "the prompt was never counted, neither evaluated nor cached"
    );
}

/// The three protocols are asked the same question and must answer with the
/// same shape, since everything downstream reads only that shape.
#[tokio::test]
async fn every_provider_answers_in_the_same_shape() {
    use futures::StreamExt;

    let mock = start_mock().await;
    let providers: Vec<(&str, Box<dyn LlmProvider>)> = vec![
        (
            "anthropic",
            Box::new(AnthropicProvider::new(mock.base_url.clone(), "k".into())),
        ),
        (
            "gemini",
            Box::new(GeminiProvider::new(
                mock.base_url.clone(),
                "k".into(),
                "mock".into(),
            )),
        ),
    ];

    for (name, provider) in providers {
        let mut stream = provider
            .chat_completion_stream(&ask("hello"))
            .await
            .unwrap_or_else(|e| panic!("{name} should stream: {e:?}"));

        let mut text = String::new();
        let mut usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap_or_else(|e| panic!("{name} chunk: {e:?}"));
            assert_eq!(chunk.object, "chat.completion.chunk", "{name} envelope");
            assert_eq!(chunk.choices.len(), 1, "{name} should answer once");
            if let Some(content) = &chunk.choices[0].delta.content {
                text.push_str(content);
            }
            if let Some(u) = chunk.usage {
                usage = Some(u);
            }
        }

        assert!(!text.is_empty(), "{name} produced no text");
        let usage = usage.unwrap_or_else(|| panic!("{name} never reported usage"));
        // Evaluated or served from cache, but accounted for either way: these
        // two ask the same question, so whichever runs second is a cache hit
        // and legitimately evaluates nothing.
        let cached = usage
            .prompt_tokens_details
            .as_ref()
            .map(|d| d.cached_tokens)
            .unwrap_or(0);
        assert!(
            usage.prompt_tokens + cached > 0,
            "{name} accounted for none of the prompt, neither evaluated nor cached"
        );
        assert!(
            usage.completion_tokens > 0,
            "{name} reported no completion tokens"
        );
    }
}

/// An Anthropic endpoint that declines, as reasoning extraction, any request
/// carrying earlier thinking, and answers one without it -- what the real
/// API did, every time, to "Why did you do it that way?".
async fn refusing_endpoint(
    category: &'static str,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    use axum::{Json, Router, routing::post};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<serde_json::Value>| {
            let log = log.clone();
            async move {
                let thinks = body["messages"].as_array().unwrap().iter().any(|m| {
                    m["content"]
                        .as_array()
                        .is_some_and(|c| c.iter().any(|b| b["type"] == "thinking"))
                });
                log.lock().unwrap().push(body);
                let start = r#"{"type":"message_start","message":{"id":"m","model":"claude-sonnet-5-5","usage":{"input_tokens":7,"output_tokens":1}}}"#;
                let events: Vec<String> = if thinks {
                    vec![
                        start.into(),
                        format!(
                            r#"{{"type":"message_delta","delta":{{"stop_reason":"refusal","stop_details":{{"type":"refusal","category":"{category}","explanation":"Blocked. API integrators: see docs."}}}},"usage":{{"output_tokens":0}}}}"#
                        ),
                        r#"{"type":"message_stop"}"#.into(),
                    ]
                } else {
                    vec![
                        start.into(),
                        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.into(),
                        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Fine."}}"#.into(),
                        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#.into(),
                        r#"{"type":"message_stop"}"#.into(),
                    ]
                };
                let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn asked_why_with_earlier_thinking() -> ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": "claude-sonnet-5-5",
        "messages": [
            {"role": "user", "content": "When?"},
            {"role": "assistant", "content": "",
             "thinking_blocks": [{"type": "thinking", "thinking": "Check.", "signature": "c2ln"}],
             "tool_calls": [{"id": "c1", "type": "function",
                             "function": {"name": "clock", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "noon"},
            {"role": "user", "content": "Thanks. Why did you do it that way?"},
        ],
    }))
    .unwrap()
}

async fn drain(
    provider: &AnthropicProvider,
) -> (String, Option<String>, Option<outturn::llm::Usage>) {
    use futures::StreamExt;
    let mut stream = provider
        .chat_completion_stream(&asked_why_with_earlier_thinking())
        .await
        .expect("stream");
    let (mut text, mut refusal, mut usage) = (String::new(), None, None);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("chunk");
        text.push_str(
            chunk.choices[0]
                .delta
                .content
                .as_deref()
                .unwrap_or_default(),
        );
        if let Some(why) = &chunk.choices[0].delta.refusal {
            refusal = Some(why.clone());
        }
        if chunk.usage.is_some() {
            usage = chunk.usage;
        }
    }
    (text, refusal, usage)
}

/// Declined as reasoning extraction before saying anything, with earlier
/// thinking in the request: asked once more without it, and the answer says
/// so on its usage, with what the declined attempt cost.
#[tokio::test]
async fn a_reasoning_extraction_refusal_is_asked_again_without_thinking() {
    let (url, seen) = refusing_endpoint("reasoning_extraction").await;
    let provider = AnthropicProvider::new(url, "k".into());

    let (text, refusal, usage) = drain(&provider).await;

    assert_eq!(text, "Fine.");
    assert!(refusal.is_none(), "the declined attempt leaked through");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "asked once, then once more");
    assert!(!seen[1].to_string().contains(r#""type":"thinking""#));
    let note = &usage.expect("usage").extra["outturn"];
    assert_eq!(note["retried_without_thinking"], "reasoning_extraction");
    assert_eq!(note["declined_attempt"]["prompt_tokens"], 7);
}

/// Any other refusal is the provider's answer, and is passed on as one.
#[tokio::test]
async fn another_refusal_is_not_asked_again() {
    let (url, seen) = refusing_endpoint("cyber").await;
    let provider = AnthropicProvider::new(url, "k".into());

    let (text, refusal, _) = drain(&provider).await;

    assert!(text.is_empty());
    assert_eq!(refusal.as_deref(), Some("Blocked."));
    assert_eq!(seen.lock().unwrap().len(), 1);
}
