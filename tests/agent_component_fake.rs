//! Component behaviour against a scripted gateway.
//!
//! These need no model, so they are fast and deterministic. The live-gateway
//! test in agent_component.rs covers the real wire format; these cover what a
//! real provider makes awkward to arrange -- failures, truncation, and exactly
//! what the host sent.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use outturn::runtime::component::{AgentRunner, Message, RunOptions};
use uuid::Uuid;

mod common;
use common::fake_gateway::{Behaviour, FakeGateway};

fn component() -> Vec<u8> {
    std::fs::read("assets/agent_default.wasm").expect("component fixture")
}

fn options(gateway: &FakeGateway, progress: Option<Arc<dyn Fn(&str) + Send + Sync>>) -> RunOptions {
    RunOptions {
        session_id: Uuid::now_v7(),
        gateway_url: gateway.url.clone(),
        gateway_token: "test-token".into(),
        default_model: "fake".into(),
        progress,
        on_tool: None,
        timezone: None,
        fuel: 10_000_000_000,
    }
}

fn user(text: &str) -> Vec<Message> {
    vec![Message {
        role: "user".into(),
        content: text.into(),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }]
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_deltas_and_returns_the_whole_reply() {
    let gateway = FakeGateway::start(Behaviour::Reply("one two three four".into())).await;
    let runner = AgentRunner::new().expect("runner");

    let deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let deltas = Arc::clone(&deltas);
        Arc::new(move |text: &str| deltas.lock().unwrap().push(text.to_string()))
    };

    let reply = runner
        .run(
            &component(),
            user("hello"),
            "be brief".into(),
            options(&gateway, Some(sink)),
        )
        .await
        .expect("run");

    assert_eq!(reply, "one two three four");

    let seen = deltas.lock().unwrap();
    assert!(seen.len() > 1, "expected several deltas, got {}", seen.len());
    // The browser renders deltas as they arrive and keeps the final message,
    // so a mismatch would show one thing during generation and another after.
    assert_eq!(seen.concat(), reply);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_system_prompt_leads_the_conversation() {
    let gateway = FakeGateway::start(Behaviour::Reply("ok".into())).await;
    let runner = AgentRunner::new().expect("runner");

    runner
        .run(
            &component(),
            user("hello"),
            "you are a lighthouse".into(),
            options(&gateway, None),
        )
        .await
        .expect("run");

    let sent = gateway.requests();
    assert_eq!(sent.len(), 1);
    let messages = sent[0]["messages"].as_array().expect("messages");

    // Ahead of the conversation, so editing an agent takes effect on its next
    // turn rather than only on new sessions.
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "you are a lighthouse");
    assert_eq!(messages[1]["role"], "user");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gateway_failure_surfaces_as_an_error() {
    let gateway = FakeGateway::start(Behaviour::Status(
        StatusCode::SERVICE_UNAVAILABLE,
        "no provider available".into(),
    ))
    .await;
    let runner = AgentRunner::new().expect("runner");

    let result = runner
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&gateway, None),
        )
        .await;

    let error = result.expect_err("a failing gateway must not look like success");
    assert!(
        error.to_string().contains("503"),
        "the error should say what happened: {error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_stream_returns_what_arrived() {
    // An upstream that drops mid-generation: the caller should keep the text
    // it received rather than losing the turn entirely.
    let gateway = FakeGateway::start(Behaviour::TruncateAfter {
        text: "one two three four five".into(),
        chunks: 2,
    })
    .await;
    let runner = AgentRunner::new().expect("runner");

    let reply = runner
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect("a truncated stream should still yield its text");

    assert_eq!(reply, "one two ");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_conversation_is_refused_without_calling_the_model() {
    let gateway = FakeGateway::start(Behaviour::Reply("unused".into())).await;
    let runner = AgentRunner::new().expect("runner");

    let result = runner
        .run(&component(), Vec::new(), "system only".into(), options(&gateway, None))
        .await;

    assert!(result.is_err(), "nothing to respond to should be an error");
    // And it should not have spent a model call finding that out.
    assert!(
        gateway.requests().is_empty(),
        "the guest should refuse before calling the model"
    );
}

// -- Tools --------------------------------------------------------------------

/// The whole tool loop, end to end against a scripted provider.
///
/// Covers what only shows up when the pieces run together: the host reassembles
/// a tool call arriving in fragments, the guest reads the model's reason out of
/// the arguments and announces it, the clock answers in the user's zone, and
/// the reason is stripped before the call goes back to the model.
#[tokio::test(flavor = "multi_thread")]
async fn runs_a_tool_and_answers_with_its_result() {
    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"reason":"Checking today's date for you."}"#.into(),
        reply: "It is Tuesday.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool = {
        let seen = Arc::clone(&seen);
        Arc::new(move |activity: &outturn::runtime::component::ToolActivity| {
            seen.lock()
                .unwrap()
                .push((activity.name.clone(), activity.reason.clone()));
        })
    };

    let runner = AgentRunner::new().expect("runner");
    let mut options = options(&gateway, None);
    options.on_tool = Some(on_tool);
    options.timezone = Some("Australia/Brisbane".into());

    let reply = runner
        .run(
            &component(),
            user("What day is it?"),
            "You are helpful.".into(),
            options,
        )
        .await
        .expect("run");

    assert_eq!(reply, "It is Tuesday.");

    // The guest announced the call, with the model's own reason attached --
    // reassembled from arguments that arrived seven bytes at a time.
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            "get_current_time".to_string(),
            "Checking today's date for you.".to_string()
        )]
    );

    let requests = gateway.requests();
    assert_eq!(requests.len(), 2, "one call to ask, one to answer");

    // The tool was offered on the first request.
    let offered = requests[0]["tools"][0]["function"]["name"]
        .as_str()
        .expect("a tool was offered");
    assert_eq!(offered, "get_current_time");

    // The second request carries the model's request and the answer to it.
    let messages = requests[1]["messages"].as_array().expect("messages");
    let assistant = messages
        .iter()
        .find(|m| m["tool_calls"].is_array())
        .expect("the assistant's tool call went back to the model");
    let echoed = assistant["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !echoed.contains("reason"),
        "the reason is written for the user and must not be resent to the model, got {echoed:?}"
    );

    let result = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the tool result went back to the model");
    assert_eq!(result["tool_call_id"], "call_fake_1");
    let content = result["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("Australia/Brisbane"),
        "the clock answers in the user's zone, got {content:?}"
    );
}

/// Without a zone the clock says so rather than passing off UTC as local.
#[tokio::test(flavor = "multi_thread")]
async fn clock_falls_back_to_utc_when_the_zone_is_unknown() {
    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"reason":"Need the date."}"#.into(),
        reply: "Done.".into(),
    })
    .await;

    let runner = AgentRunner::new().expect("runner");
    let mut options = options(&gateway, None);
    // An unparseable zone must degrade the same way an absent one does; a
    // client sending nonsense should not put the agent in a random timezone.
    options.timezone = Some("Mars/Olympus_Mons".into());

    runner
        .run(&component(), user("When?"), String::new(), options)
        .await
        .expect("run");

    let requests = gateway.requests();
    let content = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        content.contains("UTC"),
        "an unknown zone must read as UTC, got {content:?}"
    );
}
