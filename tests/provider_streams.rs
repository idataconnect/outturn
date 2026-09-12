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
use outturn::gateway::llm::types::{ChatCompletionRequest, Message, MessageContent, Role};

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

    let base_url = format!("http://127.0.0.1:{port}");
    for _ in 0..200 {
        if reqwest::get(format!("{base_url}/healthz")).await.is_ok() {
            return Mock { child, base_url };
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the mock never became ready");
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

    assert!(chunks > 2, "a reply that arrived in one chunk did not stream");
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
                if let Some(n) = &function.name {
                    if !n.is_empty() {
                        name = Some(n.clone());
                    }
                }
                if let Some(args) = &function.arguments {
                    arguments.push_str(args);
                }
            }
        }
    }

    assert_eq!(name.as_deref(), Some("get_current_time"));
    assert_eq!(finish.as_deref(), Some("tool_calls"), "a tool call is not an ordinary stop");
    assert!(
        serde_json::from_str::<serde_json::Value>(&arguments).is_ok(),
        "the fragments did not reassemble into valid json: {arguments:?}"
    );
}
