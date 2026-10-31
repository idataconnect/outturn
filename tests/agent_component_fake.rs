//! Component behavior against a scripted gateway.
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

use common::fake_gateway::{Behavior, CALL_SIGNATURE, FakeGateway, thinking_block};
use outturn::runtime::component::Finished;

fn component() -> Vec<u8> {
    std::fs::read("assets/agent_default.wasm").expect("component fixture")
}

/// One runner for the whole binary.
///
/// Compiling the component is the most expensive thing these tests do, and a
/// runner each meant every test paid for it -- sixteen concurrent Cranelift
/// compiles of identical bytes, in a debug build, on however many cores are
/// left over. That starved the one test that measures elapsed time badly
/// enough to fail it: its turn finished when the suite did, not when its
/// deadline fired.
///
/// Sharing one runner is also what the runtime does, so these tests now
/// exercise the compile cache rather than routing around it.
fn runner() -> &'static AgentRunner {
    static RUNNER: std::sync::OnceLock<AgentRunner> = std::sync::OnceLock::new();
    RUNNER.get_or_init(|| AgentRunner::new().expect("runner"))
}

/// Every tool the default agent exposes.
///
/// Listed here rather than derived because a test fixture that asked the
/// component what it had would agree with it by construction, including about
/// a tool that went missing.
const ALL_TOOLS: &[&str] = &[
    "read_object",
    "describe_image",
    "write_object",
    "delete_object",
    "list_objects",
    "expand_archive",
    "create_archive",
    "render_pdf",
    "fetch_url",
    "get_current_time",
];

type Progress = Arc<dyn Fn(&str) + Send + Sync>;

fn options(gateway: &FakeGateway, progress: Option<Progress>) -> RunOptions {
    RunOptions {
        session_id: Uuid::now_v7(),
        gateway_url: gateway.url.clone(),
        gates: outturn::egress::gate::Gates::none().into(),
        gateway_token: "test-token".into(),
        gateway_token_expires_at: None,
        refresh_token: None,
        default_model: "fake".into(),
        progress,
        // Not exercised here: these suites assert what a guest does with a
        // reply, and thinking never reaches the guest.
        reasoning: None,
        writing: None,
        on_tool: None,
        on_tool_result: None,
        on_usage: None,
        on_write: None,
        on_absorbed: None,
        on_connection: None,
        on_gated: None,
        invocations: None,
        on_wait: None,
        storage: None,
        workspace_id: Uuid::now_v7(),
        agent_id: Uuid::now_v7(),
        write_scopes: vec!["session".into(), "agent".into()],
        read_scopes: vec!["session".into(), "agent".into(), "workspace".into()],
        skill_files: Vec::new(),
        timezone: None,
        reasoning_effort: None,
        signed: Vec::new(),
        temperature: None,
        traffic_type: "assistant".into(),
        max_tool_rounds: 100,
        max_written_bytes: 0,
        remaining_stored_bytes: None,
        reply_id: Uuid::now_v7(),
        // Production waits five minutes; a test cannot.
        idle_timeout: std::time::Duration::from_secs(2),
        admission: None,
        renderer_url: None,
        // Nothing reachable unless a test says so, which is the default a
        // workspace gets.
        egress: Vec::new(),
        held: None,
        fuel: 10_000_000_000,
        // Every tool offered outright, so a test about what a tool does can
        // script the call and nothing else. A production deployment defers
        // them all by default; the tests that are about deferral say so by
        // clearing this.
        eager_tools: ALL_TOOLS.iter().map(|n| n.to_string()).collect(),
        skill_operations: Vec::new(),
    }
}

fn user(text: &str) -> Vec<Message> {
    vec![Message {
        role: "user".into(),
        parts: vec![outturn::runtime::component::ContentPart::Text(text.into())],
        tool_call_id: None,
    }]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_deltas_and_returns_the_whole_reply() {
    let gateway = FakeGateway::start(Behavior::Reply("one two three four".into())).await;
    let runner = runner();

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
        .expect("run")
        .reply;

    assert_eq!(reply, "one two three four");

    let seen = deltas.lock().unwrap();
    assert!(
        seen.len() > 1,
        "expected several deltas, got {}",
        seen.len()
    );
    // The browser renders deltas as they arrive and keeps the final message,
    // so a mismatch would show one thing during generation and another after.
    assert_eq!(seen.concat(), reply);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_system_prompt_leads_the_conversation() {
    let gateway = FakeGateway::start(Behavior::Reply("ok".into())).await;
    let runner = runner();

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
    // The agent's own prompt leads, and the guest adds the one rule the model
    // is judged by underneath it -- see `TURN_RULE` in agents/default. Asserted
    // as a prefix rather than in full so the rule's wording stays the guest's
    // to change.
    let system = messages[0]["content"].as_str().expect("system content");
    assert!(
        system.starts_with("you are a lighthouse"),
        "the agent's prompt should lead the system message: {system}"
    );
    assert!(
        system.contains("ends the turn"),
        "the turn rule should reach the model: {system}"
    );
    assert_eq!(messages[1]["role"], "user");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_failure_surfaces_as_an_error() {
    let said = outturn::gateway::failure_message(Some(
        &outturn::gateway::llm::provider::ProviderError::Unavailable,
    ));
    let gateway = FakeGateway::start(Behavior::Status(StatusCode::BAD_GATEWAY, said.clone())).await;
    let runner = runner();

    let result = runner
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&gateway, None),
        )
        .await;

    let error = result.expect_err("a failing gateway must not look like success");
    // The gateway's reason, as it gave it: the status adds nothing to a
    // sentence that already says what went wrong.
    assert_eq!(
        error.to_string(),
        said,
        "the error should say what happened"
    );
}

/// A reply the model wrote and nobody can use is told apart from an outage,
/// so the turn can be failed for good rather than retried into another loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_reply_is_marked_and_an_outage_is_not() {
    let malformed = FakeGateway::start(Behavior::MalformedReply).await;
    let error = runner()
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&malformed, None),
        )
        .await
        .expect_err("a malformed reply is a failure");
    assert!(error.malformed_reply, "not marked: {error}");

    let down = FakeGateway::start(Behavior::Status(
        StatusCode::BAD_GATEWAY,
        outturn::gateway::failure_message(Some(
            &outturn::gateway::llm::provider::ProviderError::Unavailable,
        )),
    ))
    .await;
    let error = runner()
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&down, None),
        )
        .await
        .expect_err("an outage is a failure");
    assert!(
        !error.malformed_reply,
        "an outage was marked as a malformed reply"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_truncated_stream_returns_what_arrived() {
    // An upstream that drops mid-generation: the caller should keep the text
    // it received rather than losing the turn entirely.
    let gateway = FakeGateway::start(Behavior::TruncateAfter {
        text: "one two three four five".into(),
        chunks: 2,
    })
    .await;
    let runner = runner();

    let reply = runner
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect("a truncated stream should still yield its text")
        .reply;

    assert_eq!(reply, "one two ");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_conversation_is_refused_without_calling_the_model() {
    let gateway = FakeGateway::start(Behavior::Reply("unused".into())).await;
    let runner = runner();

    let result = runner
        .run(
            &component(),
            Vec::new(),
            "system only".into(),
            options(&gateway, None),
        )
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
/// Tools are named up front and their definitions fetched on demand.
///
/// The names have to be in front of the model from the first round: a model
/// cannot ask for a tool it does not know exists, so deferring the names along
/// with the schemas would make the whole set unreachable. What is deferred is
/// each definition's description and arguments, which is the bulk of the cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_names_are_offered_up_front_and_definitions_on_request() {
    let gateway = FakeGateway::start(Behavior::LoadThenToolThenReply {
        load: vec!["get_current_time".into()],
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking today's date"}"#.into(),
        reply: "It is Tuesday.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    // This test is about deferral, so nothing is offered outright.
    options.eager_tools = Vec::new();
    options.timezone = Some("Australia/Brisbane".into());

    let reply = runner
        .run(
            &component(),
            user("What day is it?"),
            "You are helpful.".into(),
            options,
        )
        .await
        .expect("run")
        .reply;

    assert_eq!(reply, "It is Tuesday.");

    let requests = gateway.requests();
    assert_eq!(requests.len(), 3, "one to load, one to call, one to answer");

    let offered = |request: &serde_json::Value| -> Vec<String> {
        request["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };

    // Round one: the loader and nothing else, since nothing is eager.
    let first = offered(&requests[0]);
    assert_eq!(
        first,
        vec!["load_tools".to_string()],
        "only the loader is offered before anything is loaded, got {first:?}"
    );

    // ...but every tool's name is in the loader's description, or the model
    // would have nothing to ask for.
    let description = requests[0]["tools"][0]["function"]["description"]
        .as_str()
        .expect("the loader describes itself");
    for name in [
        "read_object",
        "write_object",
        "delete_object",
        "list_objects",
        "fetch_url",
        "describe_image",
        "expand_archive",
        "create_archive",
        "render_pdf",
        "get_current_time",
    ] {
        assert!(
            description.contains(name),
            "{name} should be named up front, got {description:?}"
        );
    }

    // Round two: what was asked for is now on offer, and the loader is still
    // there because tools remain unloaded.
    let second = offered(&requests[1]);
    assert!(
        second.contains(&"get_current_time".to_string()),
        "the loaded tool should be offered, got {second:?}"
    );
    assert!(
        !second.contains(&"read_object".to_string()),
        "a tool nobody asked for stays deferred, got {second:?}"
    );

    // The loader reported what it loaded, so the model knows it worked.
    let messages = requests[1]["messages"].as_array().expect("messages");
    let loaded = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the loader answered");
    let content = loaded["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("get_current_time"),
        "the loader names what it loaded, got {content:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_a_tool_and_answers_with_its_result() {
    let gateway = FakeGateway::start(Behavior::LoadThenToolThenReply {
        load: vec!["get_current_time".into()],
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking today's date"}"#.into(),
        reply: "It is Tuesday.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool = {
        let seen = Arc::clone(&seen);
        Arc::new(
            move |activity: &outturn::runtime::component::ToolActivity| {
                seen.lock()
                    .unwrap()
                    .push((activity.name.clone(), activity.action.clone()));
            },
        )
    };

    let runner = runner();
    let mut options = options(&gateway, None);
    // This test is about deferral, so nothing is offered outright.
    options.eager_tools = Vec::new();
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
        .expect("run")
        .reply;

    assert_eq!(reply, "It is Tuesday.");

    // The guest announced both calls, with the model's own reason attached --
    // reassembled from arguments that arrived seven bytes at a time. Loading
    // is announced like any other call: it is something the agent did.
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            (
                "load_tools".to_string(),
                "Getting the tools ready".to_string()
            ),
            (
                "get_current_time".to_string(),
                "Checking today's date".to_string()
            )
        ]
    );

    let requests = gateway.requests();
    assert_eq!(requests.len(), 3, "one to load, one to ask, one to answer");

    // The tool was among those offered once it had been loaded. Which position
    // it holds is not meaningful, and asserting one made this break the
    // moment another tool was added.
    let offered: Vec<&str> = requests[1]["tools"]
        .as_array()
        .expect("tools were offered")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        offered.contains(&"get_current_time"),
        "the clock should be on offer once loaded, got {offered:?}"
    );

    // The third request carries the model's request and the answer to it.
    // Both rounds are in there -- the load and the call -- so each lookup
    // names the clock's rather than taking whichever came first.
    let messages = requests[2]["messages"].as_array().expect("messages");
    let assistant = messages
        .iter()
        .find(|m| m["tool_calls"][0]["function"]["name"] == "get_current_time")
        .expect("the assistant's tool call went back to the model");
    let echoed = assistant["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !echoed.contains("action"),
        "the label is written for the user and must not be resent to the model, got {echoed:?}"
    );

    let result = messages
        .iter()
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call_fake_2")
        .expect("the tool result went back to the model");
    let content = result["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("Australia/Brisbane"),
        "the clock answers in the user's zone, got {content:?}"
    );

    // The weekday is supplied, and agrees with the timestamp beside it. A
    // model asked to derive it from the date gets it wrong -- which is the
    // whole reason the host computes it.
    let clock: serde_json::Value = serde_json::from_str(content).expect("the result is JSON");
    let stamp = clock["now"].as_str().expect("now");
    let parsed = chrono::DateTime::parse_from_rfc3339(stamp).expect("now is RFC 3339");
    assert_eq!(
        clock["weekday"].as_str().expect("weekday"),
        parsed.format("%A").to_string(),
        "weekday must match the timestamp it is sent with"
    );
    assert!(
        !stamp.contains('.'),
        "fractional seconds are noise in a prompt, got {stamp:?}"
    );
    assert_eq!(clock["abbreviation"], "AEST");
}

/// Without a zone the clock says so rather than passing off UTC as local.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clock_falls_back_to_utc_when_the_zone_is_unknown() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Reading the clock"}"#.into(),
        reply: "Done.".into(),
    })
    .await;

    let runner = runner();
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

/// A reply that thought and then called a tool goes back on the next round
/// with its thinking and its call's signature, which the guest never saw:
/// without them the model starts each round without the reasons for the
/// call it just made, and Gemini refuses the round outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_reply_goes_back_signed() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Reading the clock"}"#.into(),
        reply: "Done.".into(),
    })
    .await;

    let finished = runner()
        .run(
            &component(),
            user("When?"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect("run");

    let requests = gateway.requests();
    let reply = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
        .expect("the reply that called the tool")
        .clone();
    assert_eq!(
        reply["thinking_blocks"],
        serde_json::json!([thinking_block()])
    );
    assert_eq!(reply["tool_calls"][0]["provider_signature"], CALL_SIGNATURE);
    assert_eq!(
        reply["signed_by"], "fake",
        "named for the model that served it, so the gateway can tell"
    );
    // Reported, so the reply keeps it for the turns after this one.
    assert_eq!(finished.signed.len(), 1);
    assert_eq!(finished.signed[0].first_call, "call_fake_1");
    assert_eq!(finished.signed[0].thinking, vec![thinking_block()]);
    assert!(
        requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m.get("thinking_blocks").is_none()),
        "nothing is signed before the model has said anything"
    );
}

/// A reply from an earlier turn comes back with what it was signed with,
/// which the API hands in beside the conversation -- and is not reported
/// again, since the API already keeps it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_earlier_turns_signed_reply_goes_back_signed() {
    use outturn::runtime::component::{ContentPart, ToolCall};
    let gateway = FakeGateway::start(Behavior::Reply("Fine.".into())).await;
    let text = |role: &str, t: &str| Message {
        role: role.into(),
        parts: vec![ContentPart::Text(t.into())],
        tool_call_id: None,
    };
    let conversation = vec![
        text("user", "When?"),
        Message {
            role: "assistant".into(),
            parts: vec![ContentPart::Call(ToolCall {
                id: "c_earlier".into(),
                name: "get_current_time".into(),
                arguments: "{}".into(),
            })],
            tool_call_id: None,
        },
        Message {
            role: "tool".into(),
            parts: vec![ContentPart::Text("noon".into())],
            tool_call_id: Some("c_earlier".into()),
        },
        text("assistant", "Noon."),
        text("user", "And now?"),
    ];
    let mut options = options(&gateway, None);
    options.signed = vec![outturn::protocol::work::SignedReply {
        first_call: "c_earlier".into(),
        model: "claude-sonnet-5-5".into(),
        thinking: vec![thinking_block()],
        calls: Default::default(),
    }];

    let finished = runner()
        .run(&component(), conversation, String::new(), options)
        .await
        .expect("run");

    let requests = gateway.requests();
    let earlier = requests[0]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["tool_calls"][0]["id"] == "c_earlier")
        .expect("the earlier reply")
        .clone();
    assert_eq!(
        earlier["thinking_blocks"],
        serde_json::json!([thinking_block()])
    );
    assert_eq!(earlier["signed_by"], "claude-sonnet-5-5");
    assert!(
        finished.signed.is_empty(),
        "handed in, so not reported back"
    );
}

/// A provider that declines ends the turn saying why, not as a reply that
/// merely stopped -- and as a failure nothing retries, since the same
/// conversation is declined the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_ends_the_turn_saying_why() {
    let why = "This request was blocked as it seems to violate the terms.";
    let gateway = FakeGateway::start(Behavior::Refuse {
        text: "Let me ".into(),
        why: why.into(),
    })
    .await;

    let err = runner()
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect_err("a refusal is not a reply");
    let refused = err.refused.expect("named as a refusal");
    assert_eq!(refused.reason, why);
    assert_eq!(refused.category.as_deref(), Some("reasoning_extraction"));
    assert_eq!(
        refused.model.as_deref(),
        Some("fake"),
        "named for who declined"
    );
}

/// A provider that accepts the request and then goes silent is abandoned.
///
/// TCP will not save us here: the connection is healthy and keepalive probes
/// are answered, so nothing below the application layer has anything to
/// complain about. Left alone the turn never ends -- and because the job
/// heartbeat renews the lease while the worker waits, it would not even be
/// reclaimed as abandoned work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_provider_is_abandoned_rather_than_waited_on_forever() {
    let gateway = FakeGateway::start(Behavior::Hang).await;
    let runner = runner();

    // Compiled before the clock starts. At the start of the suite this test
    // can be waiting on the one shared compile, which under a loaded machine
    // once took forty-seven seconds of its sixty -- the turn itself then ended
    // two seconds after it began, which is its deadline firing on time.
    // Compiling is not what is under test, and nothing about it can hang.
    runner.verify(&component()).expect("component compiles");

    // Bounded from outside rather than measured from inside. The old test
    // asserted elapsed time against a threshold, which cannot distinguish a
    // deadline that did not fire from a turn that was starved -- and starved
    // is what it was: with the suite running sixteen sandboxes at once, this
    // turn finished when the suite did, not when its own deadline expired.
    // The same test passes in three seconds run on its own.
    //
    // Nothing sharper is available from the error, either: a read timeout
    // surfaces from reqwest as "error sending request for url ...", which is
    // also what a refused connection says.
    //
    // What the test is actually for survives all of that. Without a deadline
    // this turn never ends, so completing at all is the evidence, and the
    // outer bound only has to be shorter than forever.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        runner.run(
            &component(),
            user("Are you there?"),
            String::new(),
            options(&gateway, None),
        ),
    )
    .await
    .expect("the turn never ended, so no deadline applied");

    assert!(outcome.is_err(), "a silent provider must not hang the turn");
}

// -- Limits -------------------------------------------------------------------

/// A model that keeps asking for tools is stopped, and the turn still ends
/// with something to show rather than an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_looping_model_is_bounded_and_still_answers() {
    // Asks for a tool on every single call, so only the limit ends it.
    let gateway = FakeGateway::start(Behavior::AlwaysToolCall {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking again"}"#.into(),
        content: "Working.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.max_tool_rounds = 3;

    let reply = runner
        .run(&component(), user("Go forever."), String::new(), options)
        .await
        .expect("a bounded turn still returns a reply")
        .reply;

    assert!(
        reply.contains("Working."),
        "the turn should end with what the model managed to say, got {reply:?}"
    );
    assert_eq!(
        gateway.requests().len(),
        3,
        "the model was called more times than the limit allows"
    );
}

/// The limit is the host's, not the guest's.
///
/// A component is deployed by a workspace, so a bound that lives only in guest
/// code is a suggestion. The host counts the calls it makes and refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_refuses_past_the_limit_whatever_the_guest_intends() {
    let gateway = FakeGateway::start(Behavior::AlwaysToolCall {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking again"}"#.into(),
        content: String::new(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.max_tool_rounds = 1;

    let _ = runner
        .run(&component(), user("Go forever."), String::new(), options)
        .await;

    assert_eq!(
        gateway.requests().len(),
        1,
        "one round means one model call, regardless of what the guest asks for"
    );
}

/// A guest that runs out of fuel is told apart from one that merely failed.
///
/// The difference decides whether the turn is retried. Fuel is spent by
/// executing the guest, so the same input runs out again -- and a retry pays
/// once more for every model call made before the trap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_out_of_fuel_is_recognized_as_such() {
    let gateway = FakeGateway::start(Behavior::Reply("unused".into())).await;

    let mut options = options(&gateway, None);
    options.fuel = 1_000;
    let error = runner()
        .run(&component(), user("hello"), String::new(), options)
        .await
        .expect_err("a thousand units of fuel is not enough to start a turn");

    assert!(
        error.out_of_fuel(),
        "a fuel trap read as an ordinary failure, so it would be retried: {error}"
    );
}

/// And a failure that is not fuel is not mistaken for it, so it is retried.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ordinary_failure_is_not_taken_for_running_out_of_fuel() {
    let gateway = FakeGateway::start(Behavior::Status(
        StatusCode::INTERNAL_SERVER_ERROR,
        "broken".into(),
    ))
    .await;

    let error = runner()
        .run(
            &component(),
            user("hello"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect_err("a failing gateway fails the turn");

    assert!(
        !error.out_of_fuel(),
        "a provider failure read as a fuel trap, so it would never be retried: {error}"
    );
}

/// Tool calls from a truncated reply are refused, not executed.
///
/// A "length" finish means the output was cut off at the token limit, so the
/// arguments may be incomplete. Some truncations still parse as valid JSON --
/// into something the model never meant -- which is exactly why the finish
/// reason has to be checked rather than the arguments.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_truncated_reply_does_not_get_its_tools_run() {
    let gateway = FakeGateway::start(Behavior::TruncatedToolCall {
        name: "get_current_time".into(),
        // Valid JSON, but only because the truncation happened to land here.
        arguments: r#"{"action":"Checking"#.into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.max_tool_rounds = 2;

    let _ = runner
        .run(
            &component(),
            user("What time is it?"),
            String::new(),
            options,
        )
        .await;

    // The second request carries the refusal rather than a clock reading, so
    // the model learns its call was dropped instead of acting on a result it
    // never asked for.
    let requests = gateway.requests();
    assert!(
        requests.len() >= 2,
        "the turn should continue after refusing"
    );
    let tool_result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the refusal went back to the model");
    let content = tool_result["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("cut off"),
        "the model should be told why, got {content:?}"
    );
    assert!(
        !content.contains("timezone"),
        "the clock must not have run, got {content:?}"
    );
}

// -- Steering -----------------------------------------------------------------

/// A message sent mid-turn reaches the model at the next round.
///
/// It rides the gateway's own response rather than a channel of its own, so
/// the runtime needs neither a database nor credentials to be steered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_sent_mid_turn_reaches_the_next_round() {
    let gateway = FakeGateway::start(Behavior::ToolThenSteer {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking the clock"}"#.into(),
        steer: "actually, just tell me the year".into(),
        reply: "2026.".into(),
    })
    .await;

    // Which message the guest was handed, reported when it took it -- the
    // point the reply changed course, which is where the reader is shown it.
    let absorbed = Arc::new(Mutex::new(Vec::<Uuid>::new()));
    let mut options = options(&gateway, None);
    options.on_absorbed = Some({
        let absorbed = Arc::clone(&absorbed);
        Arc::new(move |ids: &[Uuid]| absorbed.lock().expect("lock").extend_from_slice(ids))
    });

    let runner = runner();
    let reply = runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options,
        )
        .await
        .expect("run")
        .reply;

    assert_eq!(reply, "2026.");
    assert_eq!(
        *absorbed.lock().expect("lock"),
        vec![common::fake_gateway::STEER_ID.parse::<Uuid>().unwrap()],
    );

    // The second call carries the interruption, marked as having arrived
    // during the work rather than as an orderly next question.
    let requests = gateway.requests();
    assert_eq!(requests.len(), 2, "the turn continued after being steered");
    let injected = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("just tell me the year"))
        })
        .expect("the steering message went to the model");
    assert!(
        injected["content"]
            .as_str()
            .unwrap_or_default()
            .contains("mid-turn"),
        "the model should know it was interrupted, got {:?}",
        injected["content"]
    );
}

/// Two rounds of text stream in the order they are read, break included.
///
/// The break between rounds used to be emitted by the guest after `chat`
/// returned -- by which time the second round's text had already streamed, so
/// the browser saw the rounds glued together and a blank line at the end. The
/// host now inserts it before the round's first token. What streamed must
/// still equal what is returned, or the reply changes under the reader when
/// the turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rounds_are_separated_before_the_second_begins_not_after() {
    let gateway = FakeGateway::start(Behavior::TextThenSteer {
        first: "Oh, one!".into(),
        steer: "two".into(),
        reply: "Two! Yay!".into(),
    })
    .await;

    let streamed = Arc::new(Mutex::new(String::new()));
    let sink: Arc<dyn Fn(&str) + Send + Sync> = {
        let streamed = Arc::clone(&streamed);
        Arc::new(move |text: &str| streamed.lock().expect("lock").push_str(text))
    };

    let reply = runner()
        .run(
            &component(),
            user("one"),
            String::new(),
            options(&gateway, Some(sink)),
        )
        .await
        .expect("run")
        .reply;

    let streamed = streamed.lock().expect("lock").clone();
    assert_eq!(reply, "Oh, one!\n\nTwo! Yay!");
    assert_eq!(
        streamed, reply,
        "what the browser was shown must be what the transcript stores"
    );
}

/// The platform's temperature reaches the model when the guest names none.
///
/// Resolved above the runtime and handed over on the turn; the guest sends
/// no temperature of its own, so what the gateway sees is the cascade's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_turns_temperature_reaches_the_model() {
    let gateway = FakeGateway::start(Behavior::Reply("Hi.".into())).await;
    let mut opts = options(&gateway, None);
    opts.temperature = Some(0.1);

    runner()
        .run(&component(), user("hello"), String::new(), opts)
        .await
        .expect("run");

    let requests = gateway.requests();
    let sent = requests[0]["temperature"]
        .as_f64()
        .expect("temperature was not sent");
    assert!((sent - 0.1).abs() < 1e-6, "sent {sent}");
}

// -- Accounting ---------------------------------------------------------------

/// Every model call is reported on its own, as it completes.
///
/// A turn that fell back to a second provider mid-way has two of these, and a
/// turn that fails after three calls still has three -- which is what a bill
/// needs and a sum at the end cannot give.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_model_call_reports_its_own_cost() {
    let gateway = FakeGateway::start(Behavior::TextThenSteer {
        first: "One.".into(),
        steer: "and two".into(),
        reply: "Two.".into(),
    })
    .await;

    let calls = Arc::new(Mutex::new(Vec::<u32>::new()));
    let mut opts = options(&gateway, None);
    opts.on_usage = Some({
        let calls = Arc::clone(&calls);
        Arc::new(move |c: &outturn::runtime::component::CallUsage| {
            calls.lock().expect("lock").push(c.round)
        })
    });

    runner()
        .run(&component(), user("one"), String::new(), opts)
        .await
        .expect("run");

    assert_eq!(
        *calls.lock().expect("lock"),
        vec![0, 1],
        "two rounds should have reported two costs, in order"
    );
}

/// What a turn spent is counted by the host, across every round.
///
/// The guest never sees these numbers and cannot report them: asking a
/// component deployed by a workspace to declare its own spend is asking the party
/// being billed to write the invoice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_reports_what_it_spent() {
    // Two rounds: a tool call, then the answer. Each reports usage, so a
    // turn that only counted the last one would come up short.
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking the clock"}"#.into(),
        reply: "Tuesday.".into(),
    })
    .await;

    let runner = runner();
    let Finished { cost, .. } = runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect("run");

    // Two rounds at 7 full-rate prompt tokens each, with 4 read from cache
    // and 2 written beside them. The gateway already separated them, so the
    // host takes the numbers as they are: subtracting the cache again is the
    // mistake that under-billed every cached conversation.
    assert_eq!(
        cost.prompt_tokens, 14,
        "the prompt count is taken as the gateway defined it"
    );
    assert_eq!(cost.cache_read_tokens, 8);
    assert_eq!(cost.cache_write_tokens, 4);
    assert_eq!(
        cost.completion_tokens, 14,
        "a turn costs every round it made, not just the last"
    );
    // Thinking is billed as output and reported apart, so it is inside the
    // completion total as well as recorded on its own.
    assert_eq!(cost.reasoning_tokens, 6);
}

/// The numbers the gateway sends for a cached Anthropic call are taken as
/// they are. The gateway has already put Anthropic's counts into the one
/// meaning -- 80 evaluated at full rate, 20 read from cache, 5 written, with
/// the provider's own object riding beside them -- and the host that
/// subtracted the cache from the prompt count a second time, on the OpenAI
/// protocol's account, recorded this call as 60.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cached_anthropic_call_is_billed_for_what_it_evaluated() {
    let gateway = FakeGateway::start(Behavior::ReplyCosting {
        text: "Hello.".into(),
        usage: serde_json::json!({
            "prompt_tokens": 80,
            "completion_tokens": 7,
            "total_tokens": 112,
            "prompt_tokens_details": { "cached_tokens": 20, "cache_creation_tokens": 5 },
            "anthropic": {
                "input_tokens": 80, "output_tokens": 7,
                "cache_read_input_tokens": 20, "cache_creation_input_tokens": 5,
            },
        }),
    })
    .await;

    let calls = Arc::new(Mutex::new(
        Vec::<outturn::runtime::component::CallUsage>::new(),
    ));
    let mut opts = options(&gateway, None);
    opts.on_usage = Some({
        let calls = Arc::clone(&calls);
        Arc::new(move |c: &outturn::runtime::component::CallUsage| {
            calls.lock().expect("lock").push(c.clone())
        })
    });

    let Finished { cost, .. } = runner()
        .run(&component(), user("hi"), String::new(), opts)
        .await
        .expect("run");

    assert_eq!(cost.prompt_tokens, 80, "the full-rate input, not 80 - 20");
    assert_eq!(cost.cache_read_tokens, 20);
    assert_eq!(cost.cache_write_tokens, 5);
    assert_eq!(cost.completion_tokens, 7);

    let calls = calls.lock().expect("lock");
    let call = calls.first().expect("one call's cost was reported");
    assert_eq!(call.usage.prompt_tokens, 80);
    assert!(
        !call.partial && !call.estimated,
        "a finished, measured call"
    );
    assert_eq!(
        call.provider_usage.as_ref().expect("the provider's object")["anthropic"]["cache_read_input_tokens"],
        20,
        "what rides to the ledger is the gateway's object, whole"
    );
}

/// A stream cut before the provider said what it cost still costs something.
///
/// On the OpenAI protocol usage arrives only on the last chunk, which a cut
/// stream never delivers, so a stop pressed mid-answer used to bill nothing
/// -- and a row that bills nothing reads afterwards as a free turn. The host
/// estimates instead, from what it sent and what arrived, and says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_stream_without_a_cost_is_estimated_and_marked_so() {
    let gateway = FakeGateway::start(Behavior::TruncateAfter {
        text: "one two three four five".into(),
        chunks: 2,
    })
    .await;

    let calls = Arc::new(Mutex::new(
        Vec::<outturn::runtime::component::CallUsage>::new(),
    ));
    let mut opts = options(&gateway, None);
    opts.on_usage = Some({
        let calls = Arc::clone(&calls);
        Arc::new(move |c: &outturn::runtime::component::CallUsage| {
            calls.lock().expect("lock").push(c.clone())
        })
    });

    runner()
        .run(&component(), user("hello"), String::new(), opts)
        .await
        .expect("run");

    let calls = calls.lock().expect("lock");
    let call = calls.first().expect("the cut call still reports a cost");
    assert!(
        call.estimated,
        "nothing was reported, so the numbers are a guess"
    );
    assert!(call.partial, "and they are for a cut answer");
    assert!(
        call.provider_usage.is_none(),
        "no provider figure is claimed"
    );
    // "one two " is eight characters: two tokens at four a token.
    assert_eq!(call.usage.completion_tokens, 2);
    assert!(
        call.usage.prompt_tokens > 0,
        "the prompt was sent and evaluated"
    );
}

/// A tool's full result reaches the reader, and a cut-down one reaches the model.
///
/// The split is what lets a tool return a file or a table without paying for
/// it in every later prompt: the model is told enough to reason over, and the
/// browser gets the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_result_is_reported_separately_from_what_the_model_sees() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking the clock"}"#.into(),
        reply: "Tuesday.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock()
                .unwrap()
                .push((outcome.details.clone(), outcome.is_error));
        })
    };

    let runner = runner();
    let mut options = options(&gateway, None);
    options.on_tool_result = Some(on_tool_result);

    runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let reported = seen.lock().unwrap().clone();
    assert_eq!(reported.len(), 1, "the tool reported its outcome once");
    let (details, is_error) = &reported[0];
    assert!(
        details.contains("weekday"),
        "the reader gets what the tool actually produced, got {details:?}"
    );
    assert!(!is_error, "a clock reading is not a failure");
}

/// A look at an image is a paid model call, and is accounted for as one.
///
/// It used to go out beside the conversation: not billed, since the ledger is
/// written only from usage events and it produced none, and not counted, so a
/// model looping on a picture could make as many paid vision calls as it
/// liked inside one turn. Now it reports its cost under its own traffic type,
/// so the bill says what a picture cost, and it takes a round like any call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_look_at_an_image_is_billed_and_takes_a_round() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    // Three calls: the conversation asks to look, the look itself, and the
    // conversation answering with what was seen. The fake serves the second
    // and third alike, which is all a look needs.
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "describe_image".into(),
        arguments: r#"{"path":"session/cat.png","question":"what animal?","action":"Looking at the picture"}"#.into(),
        reply: "A cat.".into(),
    })
    .await;

    let calls = Arc::new(Mutex::new(Vec::<(u32, String)>::new()));
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.on_usage = Some({
        let calls = Arc::clone(&calls);
        Arc::new(move |c: &outturn::runtime::component::CallUsage| {
            calls
                .lock()
                .expect("lock")
                .push((c.round, c.traffic_type.clone()))
        })
    });
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    let key = scope::resolve(&space, "session/cat.png").unwrap();
    store
        .write(&key, b"\x89PNG\r\n\x1a\n not really a picture")
        .await
        .expect("seed");

    let Finished { cost, .. } = runner()
        .run(
            &component(),
            user("What animal is in cat.png?"),
            String::new(),
            options,
        )
        .await
        .expect("run");

    // The look went out as its own class of traffic, so a deployment can
    // route it, and reached the ledger as what it is.
    let requests = gateway.requests();
    assert_eq!(requests.len(), 3, "a look is one call to the gateway");
    assert_eq!(
        requests[1]["messages"][0]["content"][1]["type"], "image_url",
        "the second call is the look, with the image in it"
    );
    assert_eq!(
        *calls.lock().expect("lock"),
        vec![
            (0, "assistant".to_string()),
            (1, "vision".to_string()),
            (2, "assistant".to_string()),
        ],
        "every call reports its cost, and the look says it was one"
    );
    // The fake reports 7 completion tokens a call: the look is in the total.
    assert_eq!(
        cost.completion_tokens, 21,
        "what the look cost is part of what the turn cost"
    );
}

/// The round limit counts looks, or it bounds nothing: a model that may call
/// the conversation's model three times but look at pictures without limit
/// is a model with no limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_look_at_an_image_counts_toward_the_round_limit() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "describe_image".into(),
        arguments: r#"{"path":"session/cat.png","question":"what animal?","action":"Looking at the picture"}"#.into(),
        reply: "A cat.".into(),
    })
    .await;

    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    // Enough for the round that asks and the round that answers, and not
    // for a look between them -- which, if it did not count, would fit.
    options.max_tool_rounds = 2;
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    let key = scope::resolve(&space, "session/cat.png").unwrap();
    store
        .write(&key, b"\x89PNG\r\n\x1a\n not really a picture")
        .await
        .expect("seed");

    let _ = runner()
        .run(
            &component(),
            user("What animal is in cat.png?"),
            String::new(),
            options,
        )
        .await;

    // The look took the second round, so the conversation's second round
    // was refused: two calls reached the gateway, and the look's answer was
    // the model's second-round refusal rather than a description.
    let requests = gateway.requests();
    assert_eq!(
        requests.len(),
        2,
        "the look took a round, so the turn ran out after it"
    );
}

/// A stopped turn makes no more paid calls, whatever the component does. The
/// stop arrives with a round that asks to look at an image; the default guest
/// runs the round's tools before it next checks the flag, and the host refuses
/// the look rather than trusting a component to have checked first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_turn_is_refused_its_next_model_call() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenStopped {
        name: "describe_image".into(),
        arguments: r#"{"path":"session/cat.png","question":"what animal?","action":"Looking at the picture"}"#.into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    let key = scope::resolve(&space, "session/cat.png").unwrap();
    store
        .write(&key, b"\x89PNG\r\n\x1a\n not really a picture")
        .await
        .expect("seed");

    let _ = runner()
        .run(
            &component(),
            user("What animal is in cat.png?"),
            String::new(),
            options,
        )
        .await;

    assert_eq!(
        gateway.requests().len(),
        1,
        "the look was made after the turn was stopped"
    );
}

// -- Object storage -----------------------------------------------------------

/// An agent reads and writes only within its own workspace's space.
///
/// The guest is never told which workspace it belongs to, so it cannot name
/// another; and the host resolves every path rather than trusting one, so a
/// component that tries to climb out is refused rather than quietly corrected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_is_scoped_to_the_workspace_and_traversal_is_refused() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let ours = scope::Space {
        workspace_id: Uuid::now_v7(),
        agent_id: Uuid::now_v7(),
        session_id: Uuid::now_v7(),
    };
    let theirs = scope::Space {
        workspace_id: Uuid::now_v7(),
        agent_id: Uuid::now_v7(),
        session_id: Uuid::now_v7(),
    };

    // Somebody else's object, which our agent must not be able to reach.
    store
        .write(
            &scope::resolve(&theirs, "workspace/secrets.txt").unwrap(),
            b"not yours",
        )
        .await
        .expect("seed");

    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "write_object".into(),
        arguments: r#"{"path":"agent/notes/hello.txt","content":"written by the agent","action":"Saving a note"}"#.into(),
        reply: "Saved.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.workspace_id = ours.workspace_id;
    options.agent_id = ours.agent_id;
    options.session_id = ours.session_id;

    runner
        .run(&component(), user("Write a note."), String::new(), options)
        .await
        .expect("run");

    // It landed under our agent's prefix, not at the path the guest named.
    let written = store
        .read(
            &scope::resolve(&ours, "agent/notes/hello.txt").unwrap(),
            0,
            u32::MAX,
        )
        .await
        .expect("the agent's own file");
    assert_eq!(written, b"written by the agent");

    // And the neighbor's file is untouched and unreachable by name.
    assert!(
        scope::resolve(&ours, "workspace/../../{theirs}/secrets.txt").is_err(),
        "a path climbing out of the space must be refused"
    );
    let theirs_still = store
        .read(
            &scope::resolve(&theirs, "workspace/secrets.txt").unwrap(),
            0,
            u32::MAX,
        )
        .await
        .expect("still there");
    assert_eq!(theirs_still, b"not yours");
}

/// A write that would take the turn past its limit on bytes is refused before
/// anything is stored, and one inside it is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_past_the_turns_limit_is_refused_before_it_lands() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    for (limit, lands) in [(10, false), (100, true)] {
        let store = Arc::new(MemoryStorage::new());
        let gateway = FakeGateway::start(Behavior::ToolThenReply {
            name: "write_object".into(),
            arguments:
                r#"{"path":"session/big.txt","content":"twenty bytes exactly","action":"Saving"}"#
                    .into(),
            reply: "Done.".into(),
        })
        .await;
        let mut options = options(&gateway, None);
        options.storage = Some(store.clone());
        options.max_written_bytes = limit;
        let space = scope::Space {
            workspace_id: options.workspace_id,
            agent_id: options.agent_id,
            session_id: options.session_id,
        };
        runner()
            .run(&component(), user("Save it."), String::new(), options)
            .await
            .expect("run");

        let key = scope::resolve(&space, "session/big.txt").unwrap();
        assert_eq!(
            store.read(&key, 0, u32::MAX).await.is_ok(),
            lands,
            "a 20-byte write under a {limit}-byte limit"
        );
    }
}

/// A write past what the workspace has left to store is refused before it
/// lands, and the model is told the storage is full rather than that the turn
/// wrote too much.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_past_the_workspaces_quota_is_refused_as_storage_full() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "write_object".into(),
        arguments:
            r#"{"path":"session/big.txt","content":"twenty bytes exactly","action":"Saving"}"#
                .into(),
        reply: "Done.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.remaining_stored_bytes = Some(10);
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    runner()
        .run(&component(), user("Save it."), String::new(), options)
        .await
        .expect("run");

    let key = scope::resolve(&space, "session/big.txt").unwrap();
    assert!(
        store.read(&key, 0, u32::MAX).await.is_err(),
        "nothing landed"
    );
    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(result.contains("storage is full"), "{result}");
}

/// A path that names no scope is answered with how to write one.
///
/// The one storage error a model will hit most, so the refusal is a
/// correction: it names the three prefixes and shows the path under each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scopeless_path_is_corrected_not_just_refused() {
    use outturn::storage::MemoryStorage;

    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "write_object".into(),
        arguments: r#"{"path":"notes.txt","content":"x","action":"Saving"}"#.into(),
        reply: "Oh.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(Arc::new(MemoryStorage::new()));

    runner()
        .run(&component(), user("Save it."), String::new(), options)
        .await
        .expect("run");

    // The tool result the model was handed on the second call.
    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(result.contains("session/notes.txt"), "{result}");
    assert!(result.contains("agent/notes.txt"), "{result}");
    assert!(result.contains("workspace/notes.txt"), "{result}");
}

/// Writing to a scope the cascade did not allow is refused, with a way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_outside_the_allowed_scopes_are_refused() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "write_object".into(),
        arguments:
            r#"{"path":"workspace/pricing.csv","content":"cheap","action":"Updating prices"}"#
                .into(),
        reply: "Refused.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    // Session and agent only, which is the default the cascade resolves to.
    options.write_scopes = vec!["session".into(), "agent".into()];
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };

    runner()
        .run(
            &component(),
            user("Update pricing."),
            String::new(),
            options,
        )
        .await
        .expect("run");

    assert!(
        store
            .read(
                &scope::resolve(&space, "workspace/pricing.csv").unwrap(),
                0,
                u32::MAX
            )
            .await
            .is_err(),
        "the write to workspace scope went through"
    );
    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(
        result.contains("session/"),
        "the refusal should say where to write instead: {result}"
    );
}

/// Deleting removes the object, rather than leaving an empty one behind.
///
/// The guest has no filesystem, so before `delete_object` existed a model
/// asked to delete reached for the nearest thing it had -- writing nothing
/// over the file -- and then reported a deletion that had not happened, while
/// the panel went on listing a zero-byte file. Nothing listed is the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_removes_the_object_rather_than_emptying_it() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "delete_object".into(),
        arguments: r#"{"path":"session/recipe.pdf","action":"Deleting the file"}"#.into(),
        reply: "Deleted.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    let key = scope::resolve(&space, "session/recipe.pdf").unwrap();
    store
        .write(&key, b"%PDF-1.4 banana bread")
        .await
        .expect("seed");

    runner
        .run(
            &component(),
            user("Please delete that file now."),
            String::new(),
            options,
        )
        .await
        .expect("run");

    assert!(
        store.stat(&key).await.is_err(),
        "the object is still there after a delete"
    );
    let listed = store
        .list(&scope::root_for(&space, scope::Scope::Session))
        .await
        .expect("list");
    assert!(
        listed.is_empty(),
        "a deleted file must not go on listing, at any size: {listed:?}"
    );
}

/// Deleting from a scope this agent may only read is refused.
///
/// Emptying a file is a way of changing it, so the scope that governs writing
/// governs this too. A read-only workspace that an agent could delete from
/// would be read-only in name only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_outside_the_allowed_scopes_is_refused() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "delete_object".into(),
        arguments: r#"{"path":"workspace/pricing.csv","action":"Deleting the price list"}"#.into(),
        reply: "Refused.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.write_scopes = vec!["session".into(), "agent".into()];
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    let key = scope::resolve(&space, "workspace/pricing.csv").unwrap();
    store.write(&key, b"not cheap").await.expect("seed");

    runner()
        .run(
            &component(),
            user("Delete the price list."),
            String::new(),
            options,
        )
        .await
        .expect("run");

    assert!(
        store.stat(&key).await.is_ok(),
        "the refused delete went through anyway"
    );
}

/// Deleting nothing is an error, not a quiet success.
///
/// The failure this guards is a model telling someone their file is gone
/// because a tool answered "fine" to a path it never found.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_path_that_names_nothing_says_so() {
    use outturn::storage::MemoryStorage;

    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "delete_object".into(),
        arguments: r#"{"path":"session/never-existed.md","action":"Deleting the draft"}"#.into(),
        reply: "It was not there.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(Arc::new(MemoryStorage::new()));

    runner()
        .run(
            &component(),
            user("Delete the draft."),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(
        result.contains("error") && result.contains("session/never-existed.md"),
        "the model must be told the path was not there: {result}"
    );
}

/// A file too large to show comes back as both ends, not just the start.
///
/// Head-only truncation loses exactly the part that matters in a log: the
/// setup is at the top and what went wrong is at the bottom. What was skipped
/// is described precisely enough to go and fetch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_file_is_read_from_both_ends() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "read_object".into(),
        arguments: r#"{"path":"session/app.log","action":"Reading the log"}"#.into(),
        reply: "It caught fire.".into(),
    })
    .await;
    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };

    // Distinctive first and last lines, with plenty of filler between.
    let mut log = String::from("FIRST LINE: service starting\n");
    for i in 0..5000 {
        log.push_str(&format!("line {i} of unremarkable middle\n"));
    }
    log.push_str("LAST LINE: everything caught fire\n");
    store
        .write(
            &scope::resolve(&space, "session/app.log").unwrap(),
            log.as_bytes(),
        )
        .await
        .expect("seed");

    runner
        .run(&component(), user("What happened?"), String::new(), options)
        .await
        .expect("run");

    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the read came back");
    let content = result["content"].as_str().unwrap_or_default();

    assert!(
        content.contains("FIRST LINE"),
        "the beginning should survive"
    );
    assert!(
        content.contains("LAST LINE: everything caught fire"),
        "the end is the part that matters in a log, and head-only truncation \
         would have thrown it away"
    );
    assert!(
        content.contains("not shown. Read again with offset="),
        "the gap should say where to continue, got {content:.400}"
    );
}

/// A file that is not text can still be looked at, as its bytes.
///
/// A plain read refuses it and says how to look; a hex read shows the header
/// that says what it is, with the offsets beside it, and where to continue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_that_is_not_text_is_read_as_hex() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let read = async |arguments: &str| {
        let store = Arc::new(MemoryStorage::new());
        let gateway = FakeGateway::start(Behavior::ToolThenReply {
            name: "read_object".into(),
            arguments: arguments.into(),
            reply: "Looked.".into(),
        })
        .await;
        let mut options = options(&gateway, None);
        options.storage = Some(store.clone());
        let space = scope::Space {
            workspace_id: options.workspace_id,
            agent_id: options.agent_id,
            session_id: options.session_id,
        };
        // The start of a HEIC photo, then enough of nothing to need a second read.
        let mut photo = b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00mif1heic".to_vec();
        photo.resize(10_000, 0xff);
        store
            .write(&scope::resolve(&space, "session/IMG.HEIC").unwrap(), &photo)
            .await
            .expect("seed");
        runner()
            .run(&component(), user("What is it?"), String::new(), options)
            .await
            .expect("run");
        let requests = gateway.requests();
        requests[1]["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .find(|m| m["role"] == "tool")
            .expect("the read came back")["content"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };

    let plain = read(r#"{"path":"session/IMG.HEIC","action":"Reading it"}"#).await;
    assert!(
        plain.contains("format \\\"hex\\\""),
        "a refusal should say how to look anyway: {plain}"
    );

    let hex =
        read(r#"{"path":"session/IMG.HEIC","format":"hex","action":"Looking at its bytes"}"#).await;
    let dump: serde_json::Value = serde_json::from_str(&hex).expect("json");
    let content = dump["content"].as_str().unwrap_or_default();
    assert!(
        content.starts_with("00000000: 0000 0018 6674 7970 6865 6963 0000 0000  ....ftypheic...."),
        "the header should read as xxd lays it out: {content:.200}"
    );
    assert_eq!(dump["next_offset"], 4096, "where to continue: {hex:.300}");

    let tail = read(r#"{"path":"session/IMG.HEIC","format":"hex","from_end":true,"action":"Looking at its end"}"#).await;
    let tail: serde_json::Value = serde_json::from_str(&tail).expect("json");
    assert_eq!(tail["offset"], 10_000 - 4096);
    assert!(tail.get("next_offset").is_none(), "nothing follows the end");
}

// -- Egress -------------------------------------------------------------------

/// Runs one scripted tool call and returns what the tool produced.
async fn tool_result(arguments: &str, egress: Vec<outturn::egress::rule::EgressRule>) -> String {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "fetch_url".into(),
        arguments: arguments.into(),
        reply: "Done.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock().unwrap().push(outcome.details.clone());
        })
    };

    let mut options = options(&gateway, None);
    options.on_tool_result = Some(on_tool_result);
    options.egress = egress;

    runner()
        .run(&component(), user("go and look"), String::new(), options)
        .await
        .expect("run");

    let results = seen.lock().unwrap().clone();
    results.first().cloned().unwrap_or_default()
}

/// An agent reaches nothing until a workspace says otherwise.
///
/// The default matters more than any rule: a workspace who has not thought about
/// egress has not agreed to it, and an agent that could reach anything makes a
/// prompt injection into a way out with the data.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_reaches_nothing_it_was_not_allowed() {
    let result = tool_result(
        r#"{"url":"https://example.com/data","action":"Looking something up"}"#,
        Vec::new(),
    )
    .await;

    assert!(
        result.contains("not on this workspace's allowed list"),
        "an empty rule list let a request out: {result}"
    );
}

/// Every fetch goes to the gateway, whatever this tier believes about it.
///
/// The runtime cannot decide that a rule is genuine: it builds a proof from
/// the list it holds, and a proof of a rule nobody vouched for is simply a
/// proof that fails elsewhere. Checking here would be the sandbox's own host
/// vouching for itself. So the question this answers is not "is it allowed"
/// but "did it ask" -- the request must leave this tier and be judged where
/// the signed commitment can be read.
///
/// The gateway's own tests cover the judgment. Here the fake gateway serves
/// model calls and has no egress endpoint at all, so a fetch that was properly
/// handed over comes back as the refusal that endpoint's absence produces,
/// which is exactly the evidence wanted: it left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fetch_is_asked_of_the_gateway_rather_than_decided_here() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "fetch_url".into(),
        arguments: r#"{"url":"https://example.com/data","action":"Looking something up"}"#.into(),
        reply: "Done.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock().unwrap().push(outcome.details.clone());
        })
    };

    let mut options = options(&gateway, None);
    options.on_tool_result = Some(on_tool_result);
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];

    runner()
        .run(&component(), user("go and look"), String::new(), options)
        .await
        .expect("run");

    let result = seen.lock().unwrap().first().cloned().unwrap_or_default();
    assert!(
        result.contains("refused"),
        "the fetch should have been handed to the gateway: {result}"
    );
    // And nothing reached the internet on the way: the only thing this tier
    // can do with a URL now is ask somebody else about it.
    assert!(
        !result.contains("Example Domain"),
        "the runtime fetched this itself: {result}"
    );
}

/// The metadata service, which is the reason any of this exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_node_metadata_service_is_not_reachable() {
    let result = tool_result(
        r#"{"url":"http://169.254.169.254/latest/meta-data/","action":"Looking something up"}"#,
        vec![outturn::egress::rule::EgressRule {
            host: "169.254.169.254".into(),
            header: None,
            credential_env: None,
            client: None,
            credential: None,
            asker: None,
        }],
    )
    .await;

    assert!(
        result.contains("cannot be reached from an agent"),
        "the metadata service was reachable: {result}"
    );
}

/// Only http and https, so a URL cannot become a file read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_url_cannot_name_a_scheme_that_is_not_the_web() {
    let result = tool_result(
        r#"{"url":"file:///etc/passwd","action":"Looking something up"}"#,
        vec![outturn::egress::rule::EgressRule {
            host: "example.com".into(),
            header: None,
            credential_env: None,
            client: None,
            credential: None,
            asker: None,
        }],
    )
    .await;

    assert!(
        result.contains("not a scheme this can speak"),
        "a file URL was attempted: {result}"
    );
}

/// A byte range can begin inside a character, and that is not a binary file.
///
/// The tail of a large read starts wherever `size - tail_len` lands, which for
/// a file with any non-ASCII in it will sometimes be partway through a
/// multi-byte character. Refusing that read as "not text" is wrong about the
/// file, and sends the model looking for a problem that does not exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tail_that_begins_mid_character_is_still_text() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "read_object".into(),
        arguments: r#"{"path":"session/doc.txt","action":"Reading the document"}"#.into(),
        reply: "Read it.".into(),
    })
    .await;
    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };

    // Three-byte characters throughout, so wherever the tail begins it has a
    // good chance of landing inside one. Padded with a single ASCII character
    // per line so the offsets do not all align to three.
    let mut doc = String::from("FIRST LINE: 見出し\n");
    for i in 0..5000 {
        doc.push_str(&format!("行 {i} — 中身の行がここにあります\n"));
    }
    doc.push_str("LAST LINE: 終わり\n");
    store
        .write(
            &scope::resolve(&space, "session/doc.txt").unwrap(),
            doc.as_bytes(),
        )
        .await
        .expect("seed");

    runner
        .run(
            &component(),
            user("What does it say?"),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the read came back");
    let content = result["content"].as_str().unwrap_or_default();

    assert!(
        !content.contains("not text and cannot be shown"),
        "a text file was refused because a byte offset landed mid-character: \
         {content:.200}"
    );
    assert!(
        content.contains("LAST LINE: 終わり"),
        "the end of the file should survive the tail read, got {content:.300}"
    );
    // The bug this guards against returned an empty string rather than an
    // error: a range beginning mid-character has no valid prefix, so trimming
    // by `valid_up_to` alone leaves nothing and the read silently succeeds
    // with no content. Both ends have to be present for that to be ruled out.
    assert!(
        content.contains("FIRST LINE: 見出し"),
        "the start of the file was dropped, which is what an empty decode \
         looks like from here: {content:.300}"
    );
}

/// One line larger than the whole budget is refused, not cut.
///
/// A minified bundle or an encoded blob has no useful prefix: fifty kilobytes
/// of it costs the entire budget and tells the model nothing it can act on.
/// Better to spend a sentence saying what the file is and how to ask for part
/// of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_enormous_line_is_refused_rather_than_cut() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "read_object".into(),
        arguments: r#"{"path":"session/bundle.min.js","action":"Reading the bundle"}"#.into(),
        reply: "Had a look.".into(),
    })
    .await;
    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };

    // One line, no newlines, well past the byte ceiling.
    let minified = "a".repeat(200 * 1024);
    store
        .write(
            &scope::resolve(&space, "session/bundle.min.js").unwrap(),
            minified.as_bytes(),
        )
        .await
        .expect("seed");

    runner
        .run(&component(), user("What is in it?"), String::new(), options)
        .await
        .expect("run");

    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the read came back");
    let content = result["content"].as_str().unwrap_or_default();

    assert!(
        content.contains("single line"),
        "an enormous single line should be described rather than shown, got \
         {content:.200}"
    );
    assert!(
        !content.contains("aaaaaaaaaaaaaaaaaaaa"),
        "the budget was spent on a fragment of a minified file"
    );
}

/// A scope the agent may not read refuses the read, rather than serving it.
///
/// Reads used to be allowed anywhere in the space on the reasoning that an
/// agent which could not read its own workspace's reference material could not
/// do its job. That holds for the agent you want reading it and says nothing
/// about one you don't -- a triage agent beside an HR agent has no business in
/// the shared files, and before `workspace_file_access` there was no way to say
/// so short of a second workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scope_it_may_not_read_is_refused() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "read_object".into(),
        arguments: r#"{"path":"workspace/salaries.csv","action":"Reading the file"}"#.into(),
        reply: "Refused.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.write_scopes = vec!["session".into()];
    options.read_scopes = vec!["session".into()];
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    // There to be read, so the refusal is the permission and not a miss.
    store
        .write(
            &scope::resolve(&space, "workspace/salaries.csv").unwrap(),
            b"secret",
        )
        .await
        .expect("seed");

    runner()
        .run(
            &component(),
            user("Read the salaries."),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(
        !result.contains("secret"),
        "the file it may not read was served anyway: {result}"
    );
    assert!(
        result.contains("workspace/"),
        "the refusal should name the scope: {result}"
    );
}

/// Listing everything lists only what the agent may read.
///
/// An empty prefix means "everything I have", which expanded to all three
/// scopes whatever the settings said. A filename is often the sensitive part,
/// so a listing that named files in a scope the guest cannot open would leak
/// the thing the setting was turned on to protect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listing_everything_omits_a_scope_it_may_not_read() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "list_objects".into(),
        arguments: r#"{"prefix":"","action":"Listing the files"}"#.into(),
        reply: "Listed.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.write_scopes = vec!["session".into()];
    options.read_scopes = vec!["session".into()];
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    store
        .write(
            &scope::resolve(&space, "workspace/severance.csv").unwrap(),
            b"x",
        )
        .await
        .expect("seed");
    store
        .write(&scope::resolve(&space, "session/notes.txt").unwrap(), b"y")
        .await
        .expect("seed");

    runner()
        .run(
            &component(),
            user("What files are there?"),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(
        !result.contains("severance"),
        "a file in a scope it may not read was named in the listing: {result}"
    );
    assert!(
        result.contains("notes.txt"),
        "the scope it may read should still be listed: {result}"
    );
}

/// Naming the scope outright does not get round the gate either.
///
/// Filtering only the empty prefix left the obvious way in open: a guest told
/// "everything I have" omits what it may not read, and then asks for that scope
/// by name and is handed the listing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listing_a_scope_it_may_not_read_is_refused() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "list_objects".into(),
        arguments: r#"{"prefix":"workspace/","action":"Listing the shared files"}"#.into(),
        reply: "Listed.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.write_scopes = vec!["session".into()];
    options.read_scopes = vec!["session".into()];
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    store
        .write(
            &scope::resolve(&space, "workspace/severance.csv").unwrap(),
            b"x",
        )
        .await
        .expect("seed");

    runner()
        .run(
            &component(),
            user("List the shared files."),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let requests = gateway.requests();
    let result = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(
        !result.contains("severance"),
        "naming the scope listed what it may not read: {result}"
    );
}

/// A load that recognized nothing is reported as a failure.
///
/// Not an empty success: a model told the call worked goes on to use tools it
/// does not have, and a reader watching the turn sees a tick against work that
/// did not happen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_load_that_matched_nothing_is_an_error() {
    let gateway = FakeGateway::start(Behavior::LoadThenToolThenReply {
        load: vec!["fetch_the_web".into()],
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking today's date"}"#.into(),
        reply: "Done.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock()
                .unwrap()
                .push((outcome.content.clone(), outcome.is_error));
        })
    };

    let runner = runner();
    let mut options = options(&gateway, None);
    // This test is about deferral, so nothing is offered outright.
    options.eager_tools = Vec::new();
    options.on_tool_result = Some(on_tool_result);

    runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let results = seen.lock().unwrap().clone();
    let (content, is_error) = results.first().expect("the loader answered").clone();
    assert!(
        is_error,
        "a load that recognized nothing should be an error, got {content:?}"
    );
    assert!(
        content.contains("fetch_the_web"),
        "the failure should name what was not found, got {content:?}"
    );
    assert!(
        content.contains("get_current_time"),
        "and what was available instead, got {content:?}"
    );
}

/// A tool that was never loaded is refused rather than run.
///
/// Models call tools they were not given: a small model emitted `list_objects`
/// with only the loader on offer, carrying the loader's own arguments, and it ran.
/// Without this the deferral is cosmetic -- the prompt shrinks while nothing
/// is withheld -- and the arguments come from a schema the model never read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_that_was_not_loaded_is_refused() {
    // Asks for the clock directly, having loaded nothing.
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking today's date"}"#.into(),
        reply: "Never mind.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock()
                .unwrap()
                .push((outcome.content.clone(), outcome.is_error));
        })
    };

    let runner = runner();
    let mut options = options(&gateway, None);
    // This test is about deferral, so nothing is offered outright.
    options.eager_tools = Vec::new();
    options.on_tool_result = Some(on_tool_result);

    runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options,
        )
        .await
        .expect("run");

    let results = seen.lock().unwrap().clone();
    let (content, is_error) = results.first().expect("the call was answered").clone();
    assert!(
        is_error,
        "an unloaded tool should be refused, got {content:?}"
    );
    assert!(
        content.contains("load_tools"),
        "the refusal should name the way out, got {content:?}"
    );
    // And the clock must not have run: the refusal stands in for its answer.
    assert!(
        !content.contains("weekday"),
        "the tool must not have run, got {content:?}"
    );
}

/// A tool loaded on an earlier turn is still loaded on a later one.
///
/// The loaded set used to be turn state, so a model that read a schema and
/// then called the tool a few turns later was told to load it again -- a
/// wasted round to reload something still in front of it, which reads as the
/// platform forgetting what it just did. It is rebuilt from the transcript
/// instead, the guest being instantiated fresh for every turn with nowhere to
/// carry it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_loaded_on_an_earlier_turn_stays_loaded() {
    // Calls the clock directly, without loading it first: the load is behind
    // it, in the history.
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking today's date"}"#.into(),
        reply: "It is Saturday.".into(),
    })
    .await;

    let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock()
                .unwrap()
                .push((outcome.content.clone(), outcome.is_error));
        })
    };

    // An earlier turn that loaded the clock, as the transcript holds it: the
    // assistant's call, and the answer it got.
    let mut conversation = user("What day is it?");
    conversation.push(Message {
        role: "assistant".into(),
        parts: vec![outturn::runtime::component::ContentPart::Call(
            outturn::runtime::component::ToolCall {
                id: "call_1".into(),
                name: "load_tools".into(),
                arguments: r#"{"names":["get_current_time"]}"#.into(),
            },
        )],
        tool_call_id: None,
    });
    conversation.push(Message {
        role: "tool".into(),
        parts: vec![outturn::runtime::component::ContentPart::Text(
            r#"{"loaded":["get_current_time"]}"#.into(),
        )],
        tool_call_id: Some("call_1".into()),
    });
    conversation.extend(user("And what day is it now?"));

    let runner = runner();
    let mut options = options(&gateway, None);
    // Nothing is offered outright, so the only way the clock is callable is
    // the load in the history.
    options.eager_tools = Vec::new();
    options.on_tool_result = Some(on_tool_result);

    let reply = runner
        .run(&component(), conversation, String::new(), options)
        .await
        .expect("run")
        .reply;

    let results = seen.lock().unwrap().clone();
    let (content, is_error) = results.first().expect("the call was answered").clone();
    assert!(
        !is_error,
        "a tool loaded on an earlier turn should run, got {content:?}"
    );
    assert_eq!(reply, "It is Saturday.");

    // And it was offered on the first round, not merely accepted when called:
    // a model cannot call what it was not shown.
    let requests = gateway.requests();
    let offered: Vec<&str> = requests[0]["tools"]
        .as_array()
        .expect("tools were offered")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        offered.contains(&"get_current_time"),
        "the previously loaded tool should be on offer, got {offered:?}"
    );
}

/// An eager tool is callable without being loaded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_eager_tool_needs_no_loading() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking today's date"}"#.into(),
        reply: "It is Saturday.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    // What a deployment sets when a tool is worth offering outright.
    options.eager_tools = vec!["get_current_time".into()];

    let reply = runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options,
        )
        .await
        .expect("run")
        .reply;

    assert_eq!(reply, "It is Saturday.");
    let requests = gateway.requests();
    let offered: Vec<&str> = requests[0]["tools"]
        .as_array()
        .expect("tools were offered")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        offered.contains(&"get_current_time"),
        "an eager tool is offered from the first round, got {offered:?}"
    );
}

// Skill files ------------------------------------------------------------------

/// Runs one scripted tool call against a turn bound to one skill file, stored
/// under a workspace other than the turn's -- the operator's case -- and
/// returns what the tool answered.
async fn with_skill_file(
    store: Arc<outturn::storage::MemoryStorage>,
    tool: &str,
    arguments: &str,
    read_scopes: Vec<String>,
) -> String {
    use outturn::storage::{StorageBackend, scope};

    let operator = Uuid::now_v7();
    store
        .write(
            &scope::skill_blob_key(operator, "abc123"),
            b"POST /bookings",
        )
        .await
        .unwrap();
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: tool.into(),
        arguments: arguments.into(),
        reply: "Done.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store);
    options.read_scopes = read_scopes;
    options.skill_files = ["book", "refund"]
        .into_iter()
        .map(|op| scope::SkillObject {
            path: format!("skill/inn/{op}.md"),
            workspace_id: operator,
            sha256: "abc123".into(),
            bytes: 14,
        })
        .collect();

    runner()
        .run(&component(), user("Go."), String::new(), options)
        .await
        .expect("run");
    let requests = gateway.requests();
    requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string()
}

/// A bound skill's file is readable by its skill path whatever the turn's
/// scopes, from under the workspace that owns the skill.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_skills_file_is_read_by_its_skill_path() {
    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store,
        "read_object",
        r#"{"path":"skill/inn/book.md","action":"Reading how to book"}"#,
        vec!["session".into()],
    )
    .await;
    assert!(result.contains("POST /bookings"), "{result}");
}

/// Listing `skill/` shows the bound files, as the agent names them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_files_are_listed_under_skill() {
    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store,
        "list_objects",
        r#"{"prefix":"skill/","action":"Looking"}"#,
        vec!["session".into()],
    )
    .await;
    assert!(result.contains("skill/inn/book.md"), "{result}");
}

/// A skill file is part of what the agent was told, and cannot be rewritten.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skill_file_cannot_be_written() {
    use outturn::storage::StorageBackend;

    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store.clone(),
        "write_object",
        r#"{"path":"skill/inn/book.md","content":"GET /free-rooms","action":"Editing"}"#,
        vec!["session".into()],
    )
    .await;
    assert!(result.contains("read-only"), "{result}");
    let keys: Vec<String> = store
        .list("")
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(keys.len(), 1, "something was written: {keys:?}");
}

/// Only the files the turn was sent are reachable; a name outside the table
/// is refused, and the refusal says what is there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skill_path_not_bound_is_refused() {
    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store,
        "read_object",
        r#"{"path":"skill/inn/cancel.md","action":"Reading"}"#,
        vec!["session".into(), "agent".into(), "workspace".into()],
    )
    .await;
    assert!(result.contains("skill/inn/book.md"), "{result}");
    assert!(!result.contains("POST"), "{result}");
}

/// A model that reads a skill's operation names as tools and asks the loader
/// for one is told what it is and where it is documented, rather than only
/// that no such tool exists -- which a weaker model reads as the skill being
/// broken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loading_a_skill_operation_points_at_its_file() {
    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store,
        "load_tools",
        r#"{"names":["book"]}"#,
        vec!["session".into()],
    )
    .await;
    assert!(result.contains("skill/inn/book.md"), "{result}");
    assert!(result.contains("fetch_url"), "{result}");
}

/// Several at once are named in one sentence rather than one per name: a model
/// that took a skill's manifest for tools asks for all of it, and four copies
/// of how to read a file bury which files to read. Nor is every name and every
/// tool spelt out again after it, when the result's fields already carry them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loading_several_skill_operations_names_them_once() {
    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store,
        "load_tools",
        r#"{"names":["book","refund"]}"#,
        vec!["session".into()],
    )
    .await;
    let error = serde_json::from_str::<serde_json::Value>(&result).expect("json")["error"]
        .as_str()
        .expect("error")
        .to_string();
    assert!(error.contains("book and refund are not tools"), "{error}");
    assert!(error.contains("skill/inn/<name>.md"), "{error}");
    assert_eq!(error.matches("fetch_url").count(), 1, "{error}");
    assert!(!error.contains("Available"), "{error}");
}

/// The same when the model skips the loader and calls the operation as if it
/// were a tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calling_a_skill_operation_points_at_its_file() {
    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store,
        "book",
        r#"{"room":"orchard"}"#,
        vec!["session".into()],
    )
    .await;
    assert!(result.contains("skill/inn/book.md"), "{result}");
}

/// The model beginning a call is said as soon as the call has a name, once, and
/// before the guest starts it -- which is what ends a thought's clock where the
/// model stopped thinking, rather than seconds later when the call was whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_being_written_is_said_before_it_starts() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking today's date"}"#.into(),
        reply: "It is Tuesday.".into(),
    })
    .await;

    // One log for both sinks, so the order between them is what is asserted.
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let writing = {
        let log = Arc::clone(&log);
        Arc::new(move |index: u32, name: &str, bytes: u64| {
            log.lock()
                .unwrap()
                .push(format!("writing {index} {name} {bytes}"));
        }) as outturn::runtime::component::WritingSink
    };
    let on_tool = {
        let log = Arc::clone(&log);
        Arc::new(
            move |activity: &outturn::runtime::component::ToolActivity| {
                log.lock()
                    .unwrap()
                    .push(format!("started {}", activity.name));
            },
        )
    };

    let mut options = options(&gateway, None);
    options.eager_tools = vec!["get_current_time".into()];
    options.writing = Some(writing);
    options.on_tool = Some(on_tool);

    runner()
        .run(
            &component(),
            user("What day is it?"),
            "You are helpful.".into(),
            options,
        )
        .await
        .expect("run");

    // Arguments arrive seven bytes at a time; the name is said once, first,
    // with nothing written yet. Progress after it is a second apart, and this
    // call is written in less.
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "writing 0 get_current_time 0".to_string(),
            "started get_current_time".to_string(),
        ]
    );
}

/// The tool result the model was handed after the first round's tool call.
fn first_tool_result(gateway: &FakeGateway) -> String {
    gateway.requests()[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result")["content"]
        .as_str()
        .expect("content")
        .to_string()
}

/// A stand-in for the renderer service: answers every render with the same
/// small PDF-shaped bytes, and remembers the markdown it was sent.
async fn fake_renderer() -> (String, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let app = axum::Router::new().route(
        "/v1/render/pdf",
        axum::routing::post({
            let seen = Arc::clone(&seen);
            move |body: String| async move {
                seen.lock().unwrap().push(body);
                b"%PDF-1.7 rendered".to_vec()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

/// A PDF asked for by name is made by the renderer and stored where the agent
/// said, without passing through the guest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_pdf_stores_what_the_renderer_made_at_the_path_named() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let (renderer, seen) = fake_renderer().await;
    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "render_pdf".into(),
        arguments: r##"{"path":"session/report.pdf","markdown":"# Aging\n\nAcme owes 1,200.00\n","action":"Making the report"}"##.into(),
        reply: "Done.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.renderer_url = Some(renderer);
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };

    runner()
        .run(
            &component(),
            user("Make the report."),
            String::new(),
            options,
        )
        .await
        .expect("run");

    assert_eq!(*seen.lock().unwrap(), ["# Aging\n\nAcme owes 1,200.00\n"]);
    let pdf = store
        .read(
            &scope::resolve(&space, "session/report.pdf").unwrap(),
            0,
            u32::MAX,
        )
        .await
        .expect("the PDF was stored");
    assert_eq!(pdf, b"%PDF-1.7 rendered");
    let result = first_tool_result(&gateway);
    assert!(
        result.contains(&format!("\"bytes\":{}", pdf.len())),
        "{result}"
    );
}

/// Where no renderer is deployed, the agent is told so plainly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_pdf_without_a_renderer_says_it_is_not_available() {
    use outturn::storage::MemoryStorage;

    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "render_pdf".into(),
        arguments: r##"{"path":"session/report.pdf","markdown":"# Hi","action":"Making it"}"##
            .into(),
        reply: "Oh.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(Arc::new(MemoryStorage::new()));

    runner()
        .run(&component(), user("Make it."), String::new(), options)
        .await
        .expect("run");

    let result = first_tool_result(&gateway);
    assert!(result.contains("not available here"), "{result}");
}

/// A model can store a small binary file byte for byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_object_stores_base64_as_the_bytes_it_encodes() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    // A PNG signature and a NUL: nothing that survives being taken for text.
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "write_object".into(),
        arguments: r#"{"path":"session/icon.bin","content_base64":"iVBORw0KGgoA","action":"Saving the icon"}"#.into(),
        reply: "Saved.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };

    runner()
        .run(&component(), user("Save it."), String::new(), options)
        .await
        .expect("run");

    let stored = store
        .read(
            &scope::resolve(&space, "session/icon.bin").unwrap(),
            0,
            u32::MAX,
        )
        .await
        .expect("stored");
    assert_eq!(stored, b"\x89PNG\r\n\x1a\n\0");
}

/// An archive is streamed from its inputs into storage, and is a zip of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_archive_streams_its_inputs_into_one_zip() {
    use outturn::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "create_archive".into(),
        arguments: r#"{"prefix":"session/in/","path":"session/out.zip","action":"Archiving"}"#
            .into(),
        reply: "Archived.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    let space = scope::Space {
        workspace_id: options.workspace_id,
        agent_id: options.agent_id,
        session_id: options.session_id,
    };
    for (name, body) in [
        ("session/in/a.txt", &b"alpha"[..]),
        ("session/in/sub/b.bin", &[0u8, 1, 2][..]),
    ] {
        store
            .write(&scope::resolve(&space, name).unwrap(), body)
            .await
            .expect("seed");
    }

    runner()
        .run(&component(), user("Archive it."), String::new(), options)
        .await
        .expect("run");

    let zip = store
        .read(
            &scope::resolve(&space, "session/out.zip").unwrap(),
            0,
            u32::MAX,
        )
        .await
        .expect("the archive was stored");
    assert!(zip.starts_with(b"PK\x03\x04"), "not a zip");
    // Names are stored uncompressed in the headers.
    for name in ["a.txt", "sub/b.bin"] {
        assert!(
            zip.windows(name.len()).any(|w| w == name.as_bytes()),
            "{name} is not in the archive"
        );
    }
    let result = first_tool_result(&gateway);
    assert!(result.contains("\"entries\":2"), "{result}");
}

/// A call with no arguments streams none. What goes back on the next round
/// is the empty object it means, not an empty string -- which is not JSON,
/// and which a provider replaying it refused on every round after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_without_arguments_goes_back_as_an_empty_object() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "get_current_time".into(),
        arguments: String::new(),
        reply: "Done.".into(),
    })
    .await;

    runner()
        .run(
            &component(),
            user("When?"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect("run");

    let requests = gateway.requests();
    let call = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
        .expect("the call, replayed")["tool_calls"][0]
        .clone();
    assert_eq!(call["function"]["arguments"], "{}", "{call}");
}

// -- Skill operations as tools --------------------------------------------------
//
// See docs/skill-operations.md. A bound skill's declared operation is a tool of
// its own, and calling it is the request its file declares -- built here, so a
// model cannot leave out the method, the content type or the body's shape.

/// An operation declared by `skill/inn/book.md`: a booking posted to a room.
fn booking() -> outturn::protocol::work::SkillOperation {
    outturn::protocol::work::SkillOperation {
        name: "inn_book".into(),
        description: "Reserve a room for a date range.".into(),
        file: "skill/inn/book.md".into(),
        method: "POST".into(),
        url: "http://inn.example.com/rooms/{room_id}/bookings".into(),
        body: vec!["guest_name".into(), "nights".into(), "notes".into()],
        schema: serde_json::json!({
            "type": "object",
            "required": ["room_id", "guest_name", "nights"],
            "properties": {
                "room_id": {"type": "string"},
                "guest_name": {"type": "string"},
                "nights": {"type": "integer"},
                "notes": {"type": "string"}
            }
        })
        .to_string(),
        ..Default::default()
    }
}

/// Runs a turn with `booking` bound and the inn reachable, answering every
/// outbound request with a 201. Returns the gateway, for what it was asked to
/// fetch, and the tool results in order.
async fn with_booking(behavior: Behavior, eager: Vec<String>) -> (FakeGateway, Vec<String>) {
    let gateway = FakeGateway::start(behavior).await;
    gateway.answer_egress(201, r#"{"id":"bk_1"}"#);

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock().unwrap().push(outcome.details.clone());
        })
    };
    let mut options = options(&gateway, None);
    options.on_tool_result = Some(on_tool_result);
    options.eager_tools = eager;
    options.skill_operations = vec![booking()];
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "inn.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];

    runner()
        .run(
            &component(),
            user("Book the garden room."),
            String::new(),
            options,
        )
        .await
        .expect("run");
    let results = seen.lock().unwrap().clone();
    (gateway, results)
}

/// Deferred like every other tool, so its name is in the loader's list.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operation_is_offered_through_the_loader() {
    let (gateway, _) = with_booking(Behavior::Reply("Hello.".into()), Vec::new()).await;
    let tools = gateway.requests()[0]["tools"].to_string();
    assert!(tools.contains("inn_book"), "{tools}");
}

/// The call is the request the file declares: the method, the path with its
/// parameter filled in, a JSON body of the declared fields typed as declared,
/// and the content type a model left out once and was refused for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calling_an_operation_makes_the_request_its_file_declares() {
    let (gateway, results) = with_booking(
        Behavior::ToolThenReply {
            name: "inn_book".into(),
            arguments:
                r#"{"room_id":"garden","guest_name":"Jo Okafor","nights":2,"action":"Booking"}"#
                    .into(),
            reply: "Booked.".into(),
        },
        vec!["inn_book".into()],
    )
    .await;

    let fetched = gateway.fetched();
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    let request = &fetched[0];
    assert_eq!(request["method"], "POST");
    assert_eq!(
        request["url"],
        "http://inn.example.com/rooms/garden/bookings"
    );
    assert!(
        request["headers"].to_string().contains("application/json"),
        "{request}"
    );
    let body: serde_json::Value =
        serde_json::from_str(request["body"].as_str().expect("a body")).expect("json");
    assert_eq!(
        body,
        serde_json::json!({"guest_name": "Jo Okafor", "nights": 2})
    );
    assert!(results[0].contains("201"), "{}", results[0]);
}

/// A request missing what it needs is not sent: half a booking that reached
/// the recipient may already have done something.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operation_missing_an_argument_is_not_sent() {
    let (gateway, results) = with_booking(
        Behavior::ToolThenReply {
            name: "inn_book".into(),
            arguments: r#"{"room_id":"garden","nights":2,"color":"red","action":"Booking"}"#.into(),
            reply: "Hm.".into(),
        },
        vec!["inn_book".into()],
    )
    .await;

    assert!(gateway.fetched().is_empty(), "{:?}", gateway.fetched());
    assert!(results[0].contains("missing guest_name"), "{}", results[0]);
    assert!(results[0].contains("color"), "{}", results[0]);
}

/// The skill's manifest lists an operation by its own name, so that is the
/// name a model asks the loader for -- and the loader gives it the tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operation_loads_by_the_name_its_skill_lists() {
    let (gateway, results) = with_booking(
        Behavior::LoadThenToolThenReply {
            load: vec!["book".into()],
            name: "inn_book".into(),
            arguments: r#"{"room_id":"garden","guest_name":"Jo","nights":1,"action":"Booking"}"#
                .into(),
            reply: "Booked.".into(),
        },
        Vec::new(),
    )
    .await;

    assert!(results[0].contains("inn_book"), "{}", results[0]);
    assert!(!results[0].contains("error"), "{}", results[0]);
    assert_eq!(
        gateway.fetched().len(),
        1,
        "the loaded tool was not callable"
    );
}

/// A value cannot add a path segment or a parameter of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_parameter_is_encoded() {
    let (gateway, _) = with_booking(
        Behavior::ToolThenReply {
            name: "inn_book".into(),
            arguments:
                r#"{"room_id":"../admin?x=1","guest_name":"Jo","nights":1,"action":"Booking"}"#
                    .into(),
            reply: "Booked.".into(),
        },
        vec!["inn_book".into()],
    )
    .await;

    let url = gateway.fetched()[0]["url"].as_str().unwrap().to_string();
    assert_eq!(
        url,
        "http://inn.example.com/rooms/..%2Fadmin%3Fx%3D1/bookings"
    );
}

/// A manifest that says operations are tools teaches a model to ask for one
/// as `<skill>_<operation>`. Where that operation is not a tool, the model is
/// pointed at its file rather than only told no such tool exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loading_an_undeclared_operation_by_its_tool_name_points_at_its_file() {
    let store = Arc::new(outturn::storage::MemoryStorage::new());
    let result = with_skill_file(
        store,
        "load_tools",
        r#"{"names":["inn_book"]}"#,
        vec!["session".into()],
    )
    .await;
    assert!(result.contains("skill/inn/book.md"), "{result}");
    assert!(result.contains("fetch_url"), "{result}");
}

/// A JSON body sent without a content type goes as JSON: no server reads an
/// unlabeled body that way, and models forget the header often enough that
/// two APIs in one week refused a body that was right.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unlabeled_json_body_is_sent_as_json() {
    let (gateway, _) = with_booking(
        Behavior::ToolThenReply {
            name: "fetch_url".into(),
            arguments: r#"{"method":"POST","url":"http://inn.example.com/pets","body":"{\"name\":\"Biscuit\"}","action":"Adding a pet"}"#.into(),
            reply: "Added.".into(),
        },
        vec!["fetch_url".into()],
    )
    .await;
    let headers = gateway.fetched()[0]["headers"].to_string();
    assert!(headers.contains("application/json"), "{headers}");
}

/// A content type the model chose is its own, and a body that is not JSON is
/// not labeled as if it were.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chosen_content_type_and_a_plain_body_are_left_alone() {
    let (gateway, _) = with_booking(
        Behavior::ToolThenReply {
            name: "fetch_url".into(),
            arguments: r#"{"method":"POST","url":"http://inn.example.com/pets","headers":{"Content-Type":"application/x-www-form-urlencoded"},"body":"name=Biscuit","action":"Adding a pet"}"#.into(),
            reply: "Added.".into(),
        },
        vec!["fetch_url".into()],
    )
    .await;
    let headers = gateway.fetched()[0]["headers"].to_string();
    assert!(headers.contains("x-www-form-urlencoded"), "{headers}");
    assert!(!headers.contains("application/json"), "{headers}");
}

/// A list in a query goes as the parameter repeated, which is what an
/// OpenAPI query array means unless the specification says otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_list_argument_is_repeated_in_the_query() {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "inn_find".into(),
        arguments: r#"{"tags":["quiet","garden view"],"action":"Searching"}"#.into(),
        reply: "Found.".into(),
    })
    .await;
    gateway.answer_egress(200, "[]");
    let mut options = options(&gateway, None);
    options.eager_tools = vec!["inn_find".into()];
    options.skill_operations = vec![outturn::protocol::work::SkillOperation {
        name: "inn_find".into(),
        file: "skill/inn/find.md".into(),
        method: "GET".into(),
        url: "http://inn.example.com/rooms".into(),
        query: vec!["tags".into()],
        schema:
            r#"{"type":"object","properties":{"tags":{"type":"array","items":{"type":"string"}}}}"#
                .into(),
        ..Default::default()
    }];
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "inn.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];
    runner()
        .run(&component(), user("Quiet rooms?"), String::new(), options)
        .await
        .expect("run");
    let url = gateway.fetched()[0]["url"].as_str().unwrap().to_string();
    assert_eq!(
        url,
        "http://inn.example.com/rooms?tags=quiet&tags=garden%20view"
    );
    let tools = gateway.requests()[0]["tools"].to_string();
    assert!(tools.contains(r#""type":"array""#), "{tools}");
}

/// The same call failing the same way a second time is pointed out, with
/// the result, rather than answered as if it were news. The action label is
/// reworded each try, so it is not part of what makes two calls the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repeated_failure_is_pointed_out() {
    let gateway = FakeGateway::start(Behavior::AlwaysToolCall {
        name: "inn_book".into(),
        arguments: r#"{"room_id":"garden","guest_name":"Jo","nights":1,"action":"Booking"}"#.into(),
        content: "Trying.".into(),
    })
    .await;
    gateway.answer_egress(500, r#"{"code":500}"#);
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock().unwrap().push(outcome.content.clone());
        })
    };
    let mut options = options(&gateway, None);
    options.on_tool_result = Some(on_tool_result);
    options.max_tool_rounds = 3;
    options.eager_tools = vec!["inn_book".into()];
    options.skill_operations = vec![booking()];
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "inn.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];
    runner()
        .run(&component(), user("Book it."), String::new(), options)
        .await
        .expect("run");
    let results = seen.lock().unwrap().clone();
    assert!(results.len() >= 2, "{results:?}");
    assert!(!results[0].contains("already failed"), "{}", results[0]);
    assert!(
        results[1].contains("already failed this way"),
        "{}",
        results[1]
    );
}

// -- Declared writes ------------------------------------------------------------
//
// See docs/idempotency.md. An operation that declares an identity is made
// once however many times it is sent: keyed for a recipient that takes a key,
// answered from the record for one that does not.

/// The record, kept in memory for these tests as the API keeps it.
#[derive(Default)]
struct Record(Mutex<std::collections::HashMap<String, outturn::protocol::work::Claimed>>);

impl outturn::runtime::component::InvocationRecord for Record {
    fn claim(
        &self,
        key: String,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<outturn::protocol::work::Claimed, String>>
                + Send,
        >,
    > {
        use outturn::protocol::work::Claimed;
        let mut held = self.0.lock().unwrap();
        let out = match held.get(&key) {
            Some(existing) => existing.clone(),
            None => {
                held.insert(
                    key,
                    Claimed::Attempted {
                        at: chrono::Utc::now(),
                    },
                );
                Claimed::New
            }
        };
        Box::pin(async move { Ok(out) })
    }

    fn settle(
        &self,
        key: String,
        response: Option<serde_json::Value>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let mut held = self.0.lock().unwrap();
        match response {
            Some(response) => {
                held.insert(
                    key,
                    outturn::protocol::work::Claimed::Completed {
                        response,
                        at: chrono::Utc::now(),
                    },
                );
            }
            None => {
                held.remove(&key);
            }
        }
        Box::pin(async {})
    }
}

/// The booking, declaring what makes one booking the same as another.
fn declared_booking() -> outturn::protocol::work::SkillOperation {
    outturn::protocol::work::SkillOperation {
        identity: vec!["guest_name".into(), "nights".into()],
        ..booking()
    }
}

async fn book_repeatedly(
    op: outturn::protocol::work::SkillOperation,
    record: Arc<Record>,
) -> (FakeGateway, Vec<String>) {
    let gateway = FakeGateway::start(Behavior::AlwaysToolCall {
        name: "inn_book".into(),
        arguments: r#"{"room_id":"garden","guest_name":"Jo","nights":2,"action":"Booking"}"#.into(),
        content: "Booking.".into(),
    })
    .await;
    gateway.answer_egress(201, r#"{"id":"bk_1"}"#);
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock().unwrap().push(outcome.details.clone());
        })
    };
    let mut options = options(&gateway, None);
    options.on_tool_result = Some(on_tool_result);
    options.max_tool_rounds = 3;
    options.eager_tools = vec!["inn_book".into()];
    options.skill_operations = vec![op];
    options.invocations = Some(record);
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "inn.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];
    runner()
        .run(&component(), user("Book it."), String::new(), options)
        .await
        .expect("run");
    let results = seen.lock().unwrap().clone();
    (gateway, results)
}

/// Sent again, a declared write is answered from the record: one booking
/// goes out, and the model is told the second was that one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_write_sent_twice_goes_out_once() {
    let (gateway, results) = book_repeatedly(declared_booking(), Arc::new(Record::default())).await;
    assert_eq!(gateway.fetched().len(), 1, "{:?}", gateway.fetched());
    assert!(results.len() >= 2, "{results:?}");
    assert!(results[1].contains("already done"), "{}", results[1]);
    assert!(
        results[1].contains("bk_1"),
        "the first answer: {}",
        results[1]
    );
}

/// An undeclared write is sent every time: its repetition may be meant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_undeclared_write_is_sent_as_often_as_asked() {
    let (gateway, _) = book_repeatedly(booking(), Arc::new(Record::default())).await;
    assert!(gateway.fetched().len() >= 2, "{:?}", gateway.fetched());
}

/// A write sent whose answer never came back is not sent again: whether it
/// landed is not known.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_with_no_answer_is_not_sent_again() {
    // Every key reads as claimed by a turn that died before it heard back.
    struct Unanswered;
    impl outturn::runtime::component::InvocationRecord for Unanswered {
        fn claim(
            &self,
            _key: String,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<outturn::protocol::work::Claimed, String>>
                    + Send,
            >,
        > {
            Box::pin(async {
                Ok(outturn::protocol::work::Claimed::Attempted {
                    at: chrono::Utc::now(),
                })
            })
        }
        fn settle(
            &self,
            _key: String,
            _response: Option<serde_json::Value>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            Box::pin(async {})
        }
    }
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "inn_book".into(),
        arguments: r#"{"room_id":"garden","guest_name":"Jo","nights":2,"action":"Booking"}"#.into(),
        reply: "Hm.".into(),
    })
    .await;
    gateway.answer_egress(201, r#"{"id":"bk_1"}"#);
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let on_tool_result = {
        let seen = Arc::clone(&seen);
        Arc::new(move |outcome: &outturn::runtime::component::ToolOutcome| {
            seen.lock().unwrap().push(outcome.details.clone());
        })
    };
    let mut options = options(&gateway, None);
    options.on_tool_result = Some(on_tool_result);
    options.eager_tools = vec!["inn_book".into()];
    options.skill_operations = vec![declared_booking()];
    options.invocations = Some(Arc::new(Unanswered));
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "inn.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];
    runner()
        .run(&component(), user("Book it."), String::new(), options)
        .await
        .expect("run");
    assert!(gateway.fetched().is_empty(), "{:?}", gateway.fetched());
    let results = seen.lock().unwrap().clone();
    assert!(results[0].contains("never came back"), "{}", results[0]);
}

/// One declared booking, made against a gateway `prepare` has set up, and
/// what the record holds for it afterwards.
async fn one_booking(
    prepare: impl FnOnce(&FakeGateway),
) -> Option<outturn::protocol::work::Claimed> {
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "inn_book".into(),
        arguments: r#"{"room_id":"garden","guest_name":"Jo","nights":2,"action":"Booking"}"#.into(),
        reply: "Hm.".into(),
    })
    .await;
    prepare(&gateway);
    let record = Arc::new(Record::default());
    let mut options = options(&gateway, None);
    options.eager_tools = vec!["inn_book".into()];
    options.skill_operations = vec![declared_booking()];
    options.invocations = Some(Arc::clone(&record) as _);
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "inn.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];
    runner()
        .run(&component(), user("Book it."), String::new(), options)
        .await
        .expect("run");
    assert_eq!(gateway.fetched().len(), 1, "{:?}", gateway.fetched());
    let held = record.0.lock().unwrap();
    assert!(held.len() <= 1, "one booking, one key");
    held.values().next().cloned()
}

/// A write the gateway says did not complete stays `attempted`, known by the
/// gateway's header and not by its sentence; one that failed otherwise is
/// released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_stays_attempted_when_the_gateway_says_it_did_not_complete() {
    let held = one_booking(|g| g.fail_egress(true, "the connection reset mid-request")).await;
    assert!(
        matches!(
            held,
            Some(outturn::protocol::work::Claimed::Attempted { .. })
        ),
        "{held:?}"
    );

    let held =
        one_booking(|g| g.fail_egress(false, "the request did not complete, in other words")).await;
    assert!(held.is_none(), "released: {held:?}");

    // Never sent is the opposite mark, and nothing can have landed.
    let held = one_booking(|g| g.fail_egress_unsent("connection refused")).await;
    assert!(held.is_none(), "released: {held:?}");

    // The gateway's own reply cut off on the way back: the recipient answered,
    // so the booking may have been made.
    let held = one_booking(|g| g.garble_egress()).await;
    assert!(
        matches!(
            held,
            Some(outturn::protocol::work::Claimed::Attempted { .. })
        ),
        "{held:?}"
    );
}

/// A recipient's 4xx releases the claim, since nothing happened; a 5xx leaves
/// it `attempted`, since a 502 or 504 often follows a write that landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_refusal_releases_a_claimed_write() {
    let held = one_booking(|g| g.answer_egress(409, r#"{"error":"room taken"}"#)).await;
    assert!(held.is_none(), "released: {held:?}");

    for status in [502, 503, 504] {
        let held = one_booking(|g| g.answer_egress(status, "upstream timed out")).await;
        assert!(
            matches!(
                held,
                Some(outturn::protocol::work::Claimed::Attempted { .. })
            ),
            "{status}: {held:?}"
        );
    }
}

/// A recipient that takes a key is sent one, the same for the same booking,
/// and dedupes itself: nothing is kept here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recipient_that_takes_a_key_is_sent_the_same_one() {
    let keyed = outturn::protocol::work::SkillOperation {
        key_field: Some("idempotency_key".into()),
        ..declared_booking()
    };
    let (gateway, _) = book_repeatedly(keyed, Arc::new(Record::default())).await;
    let fetched = gateway.fetched();
    assert!(fetched.len() >= 2, "sent each time; the recipient dedupes");
    let key = |i: usize| {
        let body: serde_json::Value =
            serde_json::from_str(fetched[i]["body"].as_str().unwrap()).unwrap();
        body["idempotency_key"].as_str().unwrap().to_string()
    };
    assert!(key(0).starts_with("outturn-"), "{}", key(0));
    assert_eq!(key(0), key(1));
}

/// A conversation parked on a refused charge, as a resumed turn is handed it:
/// the call, and the refusal answering it.
fn parked_on_a_charge() -> Vec<Message> {
    use outturn::runtime::component::{ContentPart, ToolCall};
    vec![
        Message {
            role: "user".into(),
            parts: vec![ContentPart::Text("Charge the deposit.".into())],
            tool_call_id: None,
        },
        Message {
            role: "assistant".into(),
            parts: vec![ContentPart::Call(ToolCall {
                id: "c_charge".into(),
                name: "fetch_url".into(),
                arguments: r#"{"method":"POST","url":"https://pay.example.com/charges"}"#.into(),
            })],
            tool_call_id: None,
        },
        Message {
            role: "tool".into(),
            parts: vec![ContentPart::Text(
                "this needs approval before it can go out. Do not retry this request.".into(),
            )],
            tool_call_id: Some("c_charge".into()),
        },
    ]
}

fn pay_rule() -> Vec<outturn::egress::rule::EgressRule> {
    vec![outturn::egress::rule::EgressRule {
        host: "pay.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }]
}

/// Held requests to send, each outcome pushed onto `reported` and nothing
/// told back, as when the API could not be reached.
fn held_sends(
    calls: Vec<outturn::egress::held::HeldCall>,
    reported: &Arc<Mutex<Vec<(Uuid, outturn::egress::held::Outcome)>>>,
) -> outturn::runtime::component::HeldSends {
    let reported = Arc::clone(reported);
    outturn::runtime::component::HeldSends {
        calls,
        token: "held-token".into(),
        gates: outturn::egress::gate::Gates::none().into(),
        begin: None,
        report: Some(Arc::new(move |id, outcome| {
            reported.lock().unwrap().push((id, outcome));
            Box::pin(async { None })
        })),
    }
}

fn the_charge(id: Uuid) -> outturn::egress::held::HeldCall {
    outturn::egress::held::HeldCall {
        id,
        method: "POST".into(),
        url: "https://pay.example.com/charges".into(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(r#"{"booking_id":"bk_1","amount_pence":14500}"#.into()),
        operation: None,
        tool_call_id: Some("c_charge".into()),
        approved_by: "Dana Reyes".into(),
    }
}

/// An approved request is sent before the guest runs, as it was held; and the
/// model reads what came back where the refusal was, rather than an
/// instruction to make the call itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_request_goes_before_the_guest_and_its_answer_replaces_the_refusal() {
    use outturn::egress::held::Outcome;

    let gateway = FakeGateway::start(Behavior::Reply("The deposit is paid.".into())).await;
    gateway.answer_egress(201, r#"{"charge":"ch_1"}"#);

    let reported = Arc::default();
    let held_id = Uuid::now_v7();
    let mut options = options(&gateway, None);
    options.egress = pay_rule();
    options.held = Some(held_sends(vec![the_charge(held_id)], &reported));

    let reply = runner()
        .run(&component(), parked_on_a_charge(), String::new(), options)
        .await
        .expect("run")
        .reply;
    assert_eq!(reply, "The deposit is paid.");

    // Sent once, as it was held: the model never wrote it again.
    let fetched = gateway.fetched();
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    assert_eq!(fetched[0]["url"], "https://pay.example.com/charges");
    assert_eq!(
        fetched[0]["body"],
        r#"{"booking_id":"bk_1","amount_pence":14500}"#
    );

    let reported = reported.lock().unwrap().clone();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].0, held_id);
    assert!(
        matches!(reported[0].1, Outcome::Answered { status: 201, .. }),
        "{:?}",
        reported[0].1
    );

    // What the model was shown in the refusal's place.
    let sent = serde_json::to_string(&gateway.requests()[0]["messages"]).unwrap();
    assert!(sent.contains("Approved by Dana Reyes and sent:"), "{sent}");
    assert!(sent.contains("ch_1"), "{sent}");
    assert!(!sent.contains("Do not retry"), "{sent}");
}

/// One that never left is said to have never left, and nothing pretends it
/// went: the model is told not to make the call, and the API is told why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_request_that_never_left_is_reported_as_never_sent() {
    use outturn::egress::held::Outcome;

    let gateway = FakeGateway::start(Behavior::Reply("It did not go through.".into())).await;
    gateway.fail_egress_unsent("connection refused");

    let reported = Arc::default();
    let mut options = options(&gateway, None);
    options.egress = pay_rule();
    options.held = Some(held_sends(vec![the_charge(Uuid::now_v7())], &reported));

    runner()
        .run(&component(), parked_on_a_charge(), String::new(), options)
        .await
        .expect("run");

    let reported = reported.lock().unwrap().clone();
    assert!(
        matches!(reported.as_slice(), [(_, Outcome::NeverSent { .. })]),
        "{reported:?}"
    );
    let sent = serde_json::to_string(&gateway.requests()[0]["messages"]).unwrap();
    assert!(sent.contains("could not be sent"), "{sent}");
}

/// One that went and was never answered may have landed, and is reported so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_request_with_no_answer_is_reported_as_unknown() {
    use outturn::egress::held::Outcome;

    let gateway = FakeGateway::start(Behavior::Reply("Not sure.".into())).await;
    gateway.fail_egress(true, "the connection reset");

    let reported = Arc::default();
    let mut options = options(&gateway, None);
    options.egress = pay_rule();
    options.held = Some(held_sends(vec![the_charge(Uuid::now_v7())], &reported));

    runner()
        .run(&component(), parked_on_a_charge(), String::new(), options)
        .await
        .expect("run");

    let reported = reported.lock().unwrap().clone();
    assert!(
        matches!(reported.as_slice(), [(_, Outcome::NoAnswer { .. })]),
        "{reported:?}"
    );
    let sent = serde_json::to_string(&gateway.requests()[0]["messages"]).unwrap();
    assert!(sent.contains("not known"), "{sent}");
}

/// Sent with no refusal found to put the answer in, the model is told anyway,
/// in a note, rather than left with the stale refusal to act on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_request_with_no_refusal_found_is_still_told() {
    let gateway = FakeGateway::start(Behavior::Reply("The deposit is paid.".into())).await;
    gateway.answer_egress(201, r#"{"charge":"ch_1"}"#);

    let reported = Arc::default();
    let mut options = options(&gateway, None);
    options.egress = pay_rule();
    let unfound = outturn::egress::held::HeldCall {
        tool_call_id: None,
        ..the_charge(Uuid::now_v7())
    };
    options.held = Some(held_sends(vec![unfound], &reported));

    runner()
        .run(&component(), parked_on_a_charge(), String::new(), options)
        .await
        .expect("run");

    assert_eq!(gateway.fetched().len(), 1);
    let sent = serde_json::to_string(&gateway.requests()[0]["messages"]).unwrap();
    assert!(sent.contains("Approved by Dana Reyes and sent:"), "{sent}");
    assert!(sent.contains("ch_1"), "{sent}");
}

/// Marked as going before it goes, so a pod lost before it reports leaves the
/// request sent rather than still to send; and one that cannot be marked --
/// the API refused, or could not be reached -- is not sent at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_request_is_marked_as_going_before_it_goes() {
    let gateway = FakeGateway::start(Behavior::Reply("The deposit is paid.".into())).await;
    gateway.answer_egress(201, r#"{"charge":"ch_1"}"#);

    // Marked, and then the report never arrives: sent once, and only after
    // the mark.
    let marked: Arc<Mutex<Vec<(Uuid, usize)>>> = Arc::default();
    let reported = Arc::default();
    let held_id = Uuid::now_v7();
    let mut sends = held_sends(vec![the_charge(held_id)], &reported);
    sends.begin = Some({
        let marked = Arc::clone(&marked);
        let gateway = gateway.clone();
        Arc::new(move |id| {
            marked.lock().unwrap().push((id, gateway.fetched().len()));
            Box::pin(async { true })
        })
    });
    let mut first = options(&gateway, None);
    first.egress = pay_rule();
    first.held = Some(sends);
    runner()
        .run(&component(), parked_on_a_charge(), String::new(), first)
        .await
        .expect("run");
    assert_eq!(
        *marked.lock().unwrap(),
        vec![(held_id, 0)],
        "marked before it went"
    );
    assert_eq!(gateway.fetched().len(), 1);

    // Not marked: not sent, and the model is not told it went.
    let gateway = FakeGateway::start(Behavior::Reply("Waiting.".into())).await;
    gateway.answer_egress(201, r#"{"charge":"ch_1"}"#);
    let reported: Arc<Mutex<Vec<(Uuid, outturn::egress::held::Outcome)>>> = Arc::default();
    let mut sends = held_sends(vec![the_charge(Uuid::now_v7())], &reported);
    sends.begin = Some(Arc::new(|_| Box::pin(async { false })));
    let mut second = options(&gateway, None);
    second.egress = pay_rule();
    second.held = Some(sends);
    runner()
        .run(&component(), parked_on_a_charge(), String::new(), second)
        .await
        .expect("run");
    assert!(gateway.fetched().is_empty(), "{:?}", gateway.fetched());
    assert!(reported.lock().unwrap().is_empty());
    let sent = serde_json::to_string(&gateway.requests()[0]["messages"]).unwrap();
    assert!(!sent.contains("Approved by"), "{sent}");
}

/// A banded policy reaches the gateway as the API committed it: the host
/// relays the gates it was handed, and they still hash to the root the token
/// carries once read back. A field lost on the way is every request of the
/// turn refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_banded_policy_reaches_the_gateway_as_committed() {
    use outturn::egress::amount::{Banding, Bands, Floor};
    use outturn::egress::gate::{Gate, Gates};
    use outturn::egress::policy::{PolicyGrant, Risk};
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "inn_book".into(),
        arguments: r#"{"room_id":"garden","guest_name":"Jo Okafor","nights":2,"action":"Booking"}"#
            .into(),
        reply: "Booked.".into(),
    })
    .await;
    gateway.answer_egress(201, r#"{"id":"bk_1"}"#);
    let mut options = options(&gateway, None);
    let workspace = options.workspace_id;
    let charge = Gate {
        requires: "charge".into(),
        host: "inn.example.com".into(),
        method: "POST".into(),
        path: "/charges".into(),
        identified_by: Some("booking_id".into()),
        binds: vec!["booking_id".into(), "amount_pence".into()],
    };
    let grant = PolicyGrant {
        policy_id: Uuid::now_v7(),
        version: 1,
        job_id: Uuid::now_v7(),
        gate: charge.key(workspace),
        requires: "charge".into(),
        tests: Vec::new(),
        fields: vec!["amount_pence".into()],
        per_turn: 1,
        per_day: 20,
        sum_field: None,
        sum_per_day: None,
        banding: Some(Banding {
            field: "amount_pence".into(),
            currency: "GBP".into(),
            currency_field: None,
            minor_units: 2,
            base: Risk::High,
            bands: Bands {
                low: Some(5000),
                medium: Some(20000),
            },
            floor: Floor::default(),
        }),
        max_risk: Some(Risk::Low),
        currency_per_day: Some(50000),
        path_numbers: Vec::new(),
    };
    let gates = Gates::of(vec![charge]).with_policies(vec![grant]);
    let root = gates.root(workspace);
    options.gates = gates.into();
    options.eager_tools = vec!["inn_book".into()];
    options.skill_operations = vec![booking()];
    options.egress = vec![outturn::egress::rule::EgressRule {
        host: "inn.example.com".into(),
        header: None,
        credential_env: None,
        client: None,
        credential: None,
        asker: None,
    }];
    runner()
        .run(
            &component(),
            user("Book the garden room."),
            String::new(),
            options,
        )
        .await
        .expect("run");
    let fetched = gateway.fetched();
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    let carried: Gates = serde_json::from_value(fetched[0]["gates"].clone()).unwrap();
    assert!(carried.matches(workspace, &root), "{}", fetched[0]["gates"]);
}

/// A request that went out with a person's own key is reported by the host,
/// once per host for the turn; one the gateway did not send, or one that
/// carries the agent's key, is not. docs/user-credentials.md.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_persons_key_going_out_is_reported_by_the_host() {
    let dana = Uuid::now_v7();
    let run = |answered: bool, asker: Option<Uuid>| async move {
        let gateway = FakeGateway::start(Behavior::ToolThenReply {
            name: "fetch_url".into(),
            arguments: r#"{"url":"https://books.example.com/api/invoices","action":"Reading"}"#
                .into(),
            reply: "Done.".into(),
        })
        .await;
        if answered {
            gateway.answer_egress(200, "[]");
        } else {
            gateway.fail_egress(false, "the connection was refused");
        }
        let reported: Arc<Mutex<Vec<(String, Uuid)>>> = Arc::new(Mutex::new(Vec::new()));
        let mut options = options(&gateway, None);
        options.on_connection = Some({
            let reported = Arc::clone(&reported);
            Arc::new(move |host: &str, asker: Uuid| {
                reported.lock().unwrap().push((host.to_string(), asker));
            })
        });
        options.egress = vec![outturn::egress::rule::EgressRule {
            host: "books.example.com".into(),
            header: Some("authorization".into()),
            credential_env: None,
            client: None,
            credential: Some(Uuid::now_v7()),
            asker,
        }];
        runner()
            .run(&component(), user("my invoices"), String::new(), options)
            .await
            .expect("run");
        reported.lock().unwrap().clone()
    };

    assert_eq!(
        run(true, Some(dana)).await,
        vec![("books.example.com".to_string(), dana)]
    );
    assert!(
        run(false, Some(dana)).await.is_empty(),
        "reported though nothing was sent"
    );
    assert!(
        run(true, None).await.is_empty(),
        "the agent's key reported as somebody's"
    );
}
