//! Each provider, against the real thing, on a real key.
//!
//! Everything else in this repository can be checked for free: the mock speaks
//! all three protocols over a real socket, and the translators are unit-tested
//! against events by hand. What none of that can tell you is whether a
//! provider still behaves the way it did when the translation was written. A
//! field that moves, a validation that tightens, a count that starts arriving
//! somewhere else -- those show up here or in a customer's bill.
//!
//! So this suite exists, and it is gated behind `live-providers`, which
//! nothing else implies. Running it spends money. See Cargo.toml for why that
//! is a flag somebody types rather than a consequence of having keys.
//!
//!     GEMINI_API_KEY=... cargo test --features live-providers
//!
//! A case whose key is absent fails rather than passing quietly, so run only
//! the providers you have keys for by filtering on the name:
//!
//!     GEMINI_API_KEY=... cargo test --features live-providers gemini_
//!
//! Anthropic and OpenAI take `ANTHROPIC_MODEL`, `OPENAI_MODEL` and
//! `OPENAI_BASE_URL` where the default is not what you want to spend on.
//!
//! Kept deliberately small. Every case here is a contract that was established
//! by hand once and would otherwise quietly rot -- not a general test of the
//! provider.

use futures::StreamExt;

use outturn::gateway::llm::provider::LlmProvider;
use outturn::gateway::llm::provider::anthropic::AnthropicProvider;
use outturn::gateway::llm::provider::gemini::GeminiProvider;
use outturn::gateway::llm::provider::openai::OpenAiProvider;
use outturn::gateway::llm::types::*;

/// The key for a provider, or a failure saying which one is missing.
///
/// Missing is a failure rather than a skip, and the reasoning is the same as
/// the flag's. Somebody typing `--features live-providers` is asking to be
/// told whether the providers still behave; answering "all passed" when
/// nothing ran tells them the opposite of the truth, and a test runner gives
/// a skipped case no louder a voice than a passing one. Nothing here can mark
/// itself ignored at runtime, so the choice is between quiet success and a
/// failure that says what to set. Loud wins.
///
/// Run one provider at a time with a filter:
///
///     GEMINI_API_KEY=... cargo test --features live-providers gemini_
fn key(name: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => panic!(
            "{name} is not set, so this case could not run. Set it, or select \
             only the providers you have keys for -- `cargo test --features \
             live-providers gemini_` runs Gemini's cases and nothing else."
        ),
    }
}

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        content: MessageContent::Text(text.into()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

fn weather_tool() -> Tool {
    serde_json::from_value(serde_json::json!({
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Current weather for a city",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
            },
        }
    }))
    .expect("a tool definition")
}

fn ask(model: &str, text: &str, tools: Option<Vec<Tool>>) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![user(text)],
        temperature: None,
        max_tokens: None,
        tools,
        reasoning_effort: None,
        stream: true,
        stream_options: None,
    }
}

/// What a streamed reply amounted to, assembled the way a caller assembles it.
#[derive(Default)]
struct Reply {
    text: String,
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    finish_reason: Option<String>,
    chunks: usize,
}

async fn collect(
    provider: &dyn LlmProvider,
    request: &ChatCompletionRequest,
) -> Result<Reply, String> {
    let mut stream = provider
        .chat_completion_stream(request)
        .await
        .map_err(|e| format!("the provider refused the request: {e:?}"))?;

    let mut reply = Reply::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("a chunk was an error: {e:?}"))?;
        reply.chunks += 1;

        let choice = &chunk.choices[0];
        if let Some(text) = &choice.delta.content {
            reply.text.push_str(text);
        }
        if let Some(reason) = &choice.finish_reason {
            reply.finish_reason = Some(reason.clone());
        }
        for delta in choice.delta.tool_calls.iter().flatten() {
            let slot = delta.index as usize;
            while reply.calls.len() <= slot {
                reply.calls.push(ToolCall {
                    id: String::new(),
                    tool_type: "function".into(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: String::new(),
                    },
                    provider_signature: None,
                });
            }
            let call = &mut reply.calls[slot];
            if let Some(id) = &delta.id {
                call.id = id.clone();
            }
            // Never overwritten with nothing: only one chunk of a block
            // carries the signature, and the rest carry none.
            if let Some(signature) = &delta.provider_signature {
                call.provider_signature = Some(signature.clone());
            }
            if let Some(function) = &delta.function {
                if let Some(name) = &function.name {
                    call.function.name.push_str(name);
                }
                if let Some(arguments) = &function.arguments {
                    call.function.arguments.push_str(arguments);
                }
            }
        }
        if let Some(usage) = chunk.usage {
            reply.usage = Some(usage);
        }
    }
    Ok(reply)
}

// Gemini ------------------------------------------------------------------

const GEMINI_URL: &str = "https://generativelanguage.googleapis.com";

fn gemini(model: &str) -> GeminiProvider {
    GeminiProvider::new(GEMINI_URL.into(), key("GEMINI_API_KEY"), model.into())
}

/// A tool call, its result, and a second round that answers from it.
///
/// The round trip is the whole point. Gemini hands back a signature with a
/// call and refuses a later round that does not replay it -- so this passing
/// means the signature survived capture, storage in the canonical shape, and
/// being written back out. A translation that drops it passes every offline
/// test and fails here with a 400.
#[tokio::test]
async fn gemini_survives_a_tool_round_trip() {
    let model = "gemini-3.7-flash";
    let provider = gemini(model);

    let mut request = ask(
        model,
        "What is the weather in San Francisco? Use the tool.",
        Some(vec![weather_tool()]),
    );
    let first = collect(&provider, &request).await.expect("round one");

    assert!(!first.calls.is_empty(), "the model did not call the tool");
    let call = &first.calls[0];
    assert_eq!(call.function.name, "get_weather");
    assert!(
        serde_json::from_str::<serde_json::Value>(&call.function.arguments).is_ok(),
        "arguments did not reassemble into json: {:?}",
        call.function.arguments
    );
    assert!(
        call.provider_signature.is_some(),
        "no thought signature was captured, so the next round will be refused"
    );

    // Round two: replay the call, answer it, and see whether it is accepted.
    request.messages.push(Message {
        role: Role::Assistant,
        content: MessageContent::Text(String::new()),
        name: None,
        tool_calls: Some(first.calls.clone()),
        tool_call_id: None,
    });
    request.messages.push(Message {
        role: Role::Tool,
        content: MessageContent::Text("62F and foggy".into()),
        name: Some(call.function.name.clone()),
        tool_calls: None,
        tool_call_id: Some(call.id.clone()),
    });

    let second = collect(&provider, &request)
        .await
        .expect("round two was refused, which is what a lost signature looks like");
    assert!(!second.text.is_empty(), "round two produced no answer");
}

/// Only the first call of a parallel batch is signed.
///
/// Worth pinning because the obvious reading -- that a signature belongs to
/// the turn -- leads to copying it onto every call, which is accepted and
/// silently bills the previous turn's reasoning once per copy.
#[tokio::test]
async fn gemini_signs_only_the_first_of_several_calls() {
    let model = "gemini-3.7-flash";
    let provider = gemini(model);

    let request = ask(
        model,
        "Get the weather for San Francisco, London, and Tokyo. Call the tool once for each, all in one go.",
        Some(vec![weather_tool()]),
    );
    let reply = collect(&provider, &request)
        .await
        .expect("a parallel batch");

    if reply.calls.len() < 2 {
        // The model is free to answer one at a time; that is not a failure of
        // ours, and there is nothing to learn from the run.
        eprintln!(
            "the model made {} call(s); nothing to check",
            reply.calls.len()
        );
        return;
    }

    assert!(
        reply.calls[0].provider_signature.is_some(),
        "the first call of a batch carries the signature"
    );
    for (i, call) in reply.calls.iter().enumerate().skip(1) {
        assert!(
            call.provider_signature.is_none(),
            "call {i} came back signed as well, so the rule that only the \
             first is signed no longer holds and the replay path needs \
             revisiting: {:?}",
            call.provider_signature
        );
    }
}

/// A history whose calls carry no signature -- a conversation that changed
/// models -- is rendered as prose rather than replayed, and is accepted.
#[tokio::test]
async fn gemini_accepts_a_history_it_never_signed() {
    let model = "gemini-3.7-flash";
    let provider = gemini(model);

    let mut request = ask(model, "What is the weather in San Francisco?", None);
    request.messages.push(Message {
        role: Role::Assistant,
        content: MessageContent::Text(String::new()),
        name: None,
        tool_calls: Some(vec![ToolCall {
            id: "c1".into(),
            tool_type: "function".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"San Francisco"}"#.into(),
            },
            // From a model that is no longer answering.
            provider_signature: None,
        }]),
        tool_call_id: None,
    });
    request.messages.push(Message {
        role: Role::Tool,
        content: MessageContent::Text("62F and foggy".into()),
        name: Some("get_weather".into()),
        tool_calls: None,
        tool_call_id: Some("c1".into()),
    });
    request
        .messages
        .push(user("Given that, what should I wear?"));

    let reply = collect(&provider, &request)
        .await
        .expect("an unsigned history was refused rather than rendered");

    assert!(!reply.text.is_empty(), "no answer came back");
    let lower = reply.text.to_lowercase();
    assert!(
        lower.contains("layer") || lower.contains("jacket") || lower.contains("62"),
        "the model did not use the tool result it was shown: {}",
        reply.text
    );
}

/// Thinking tokens are billed at output rates and are not inside the answer's
/// count, so they are carried separately. If Google folds them in, this
/// catches it before a bill does.
#[tokio::test]
async fn gemini_reports_thinking_apart_from_the_answer() {
    let model = "gemini-3.7-flash";
    let provider = gemini(model);

    let request = ask(
        model,
        "In one sentence: why does fog form in San Francisco?",
        None,
    );
    let reply = collect(&provider, &request).await.expect("a reply");

    let usage = reply.usage.expect("no usage was reported at all");
    assert!(usage.prompt_tokens > 0, "the prompt was never counted");
    assert!(usage.completion_tokens > 0, "the answer was never counted");

    let raw = &usage.extra["gemini"];
    if let Some(thoughts) = raw["thoughtsTokenCount"].as_u64() {
        assert_eq!(
            usage
                .completion_tokens_details
                .as_ref()
                .map(|d| d.reasoning_tokens),
            Some(thoughts as u32),
            "thinking was reported but did not reach the canonical shape"
        );
        assert_ne!(
            u64::from(usage.completion_tokens),
            thoughts + raw["candidatesTokenCount"].as_u64().unwrap_or(0),
            "thinking looks to have been folded into the answer's count, \
             which double-counts it"
        );
    }
}

// Anthropic ---------------------------------------------------------------

const ANTHROPIC_URL: &str = "https://api.anthropic.com";

/// Anthropic names the input side before any text and the output side as it
/// grows, which is what leaves real numbers behind when a turn is stopped.
#[tokio::test]
async fn anthropic_reports_usage_as_it_goes() {
    let api_key = key("ANTHROPIC_API_KEY");
    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or("claude-haiku-4-5-20251001".into());
    let provider = AnthropicProvider::new(ANTHROPIC_URL.into(), api_key);

    let mut request = ask(
        &model,
        "In one sentence: why does fog form in San Francisco?",
        None,
    );
    request.max_tokens = Some(256);

    let reply = collect(&provider, &request).await.expect("a reply");
    assert!(reply.chunks > 2, "the reply did not arrive in pieces");
    assert!(!reply.text.is_empty(), "no text came back");

    let usage = reply.usage.expect("no usage was reported");
    assert!(
        usage.prompt_tokens > 0,
        "the input count was lost on the way"
    );
    assert!(
        usage.completion_tokens > 1,
        "the output count is still the placeholder from message_start, so a \
         turn stopped early would bill one token"
    );
}

/// A tool call whose arguments arrive as fragments, reassembled by the caller.
#[tokio::test]
async fn anthropic_streams_a_tool_call_in_fragments() {
    let api_key = key("ANTHROPIC_API_KEY");
    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or("claude-haiku-4-5-20251001".into());
    let provider = AnthropicProvider::new(ANTHROPIC_URL.into(), api_key);

    let mut request = ask(
        &model,
        "What is the weather in San Francisco? Use the tool.",
        Some(vec![weather_tool()]),
    );
    request.max_tokens = Some(256);

    let reply = collect(&provider, &request).await.expect("a reply");
    assert!(!reply.calls.is_empty(), "the model did not call the tool");
    assert_eq!(reply.calls[0].function.name, "get_weather");
    assert!(
        serde_json::from_str::<serde_json::Value>(&reply.calls[0].function.arguments).is_ok(),
        "fragments did not reassemble into json: {:?}",
        reply.calls[0].function.arguments
    );
    assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
}

// OpenAI protocol ---------------------------------------------------------

/// Usage arrives after the chunk carrying `finish_reason`, not on it.
///
/// A reader that treats the finish as the end of the stream records nothing
/// for every call, not only interrupted ones -- so this is worth knowing has
/// not changed.
#[tokio::test]
async fn openai_sends_usage_after_the_finish() {
    let api_key = key("OPENAI_API_KEY");
    let base = std::env::var("OPENAI_BASE_URL").unwrap_or("https://api.openai.com".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or("gpt-4o-mini".into());
    let provider = OpenAiProvider::new(base, Some(api_key));

    let request = ask(
        &model,
        "In one sentence: why does fog form in San Francisco?",
        None,
    );
    let reply = collect(&provider, &request).await.expect("a reply");

    assert!(!reply.text.is_empty(), "no text came back");
    assert_eq!(reply.finish_reason.as_deref(), Some("stop"));
    let usage = reply.usage.expect(
        "no usage was reported: with include_usage set it rides a chunk after \
         the finish, and a reader that stops at the finish loses it",
    );
    assert!(usage.prompt_tokens > 0);
    assert!(usage.completion_tokens > 0);
}
