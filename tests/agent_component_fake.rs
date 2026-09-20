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
use common::fake_gateway::{Behavior, FakeGateway};

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
    "fetch_url",
    "get_current_time",
];

fn options(gateway: &FakeGateway, progress: Option<Arc<dyn Fn(&str) + Send + Sync>>) -> RunOptions {
    RunOptions {
        session_id: Uuid::now_v7(),
        gateway_url: gateway.url.clone(),
        gateway_token: "test-token".into(),
        default_model: "fake".into(),
        progress,
        on_tool: None,
        on_tool_result: None,
        on_usage: None,
        on_write: None,
        storage: None,
        workspace_id: Uuid::now_v7(),
        agent_id: Uuid::now_v7(),
        write_scopes: vec!["session".into(), "agent".into()],
        read_scopes: vec!["session".into(), "agent".into(), "workspace".into()],
        timezone: None,
        reasoning_effort: None,
        temperature: None,
        traffic_type: "assistant".into(),
        max_tool_rounds: 100,
        reply_id: Uuid::now_v7(),
        // Production waits five minutes; a test cannot.
        idle_timeout: std::time::Duration::from_secs(2),
        // Nothing reachable unless a test says so, which is the default a
        // workspace gets.
        egress: Vec::new(),
        fuel: 10_000_000_000,
        // Every tool offered outright, so a test about what a tool does can
        // script the call and nothing else. A production deployment defers
        // them all by default; the tests that are about deferral say so by
        // clearing this.
        eager_tools: ALL_TOOLS.iter().map(|n| n.to_string()).collect(),
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
        .0;

    assert_eq!(reply, "one two three four");

    let seen = deltas.lock().unwrap();
    assert!(seen.len() > 1, "expected several deltas, got {}", seen.len());
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
        .expect("run")
        .0;

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
    let gateway = FakeGateway::start(Behavior::Status(
        StatusCode::SERVICE_UNAVAILABLE,
        "no provider available".into(),
    ))
    .await;
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
    assert!(
        error.to_string().contains("503"),
        "the error should say what happened: {error}"
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
        .0;

    assert_eq!(reply, "one two ");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_conversation_is_refused_without_calling_the_model() {
    let gateway = FakeGateway::start(Behavior::Reply("unused".into())).await;
    let runner = runner();

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
        .0;

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
        Arc::new(move |activity: &outturn::runtime::component::ToolActivity| {
            seen.lock()
                .unwrap()
                .push((activity.name.clone(), activity.action.clone()));
        })
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
        .0;

    assert_eq!(reply, "It is Tuesday.");

    // The guest announced both calls, with the model's own reason attached --
    // reassembled from arguments that arrived seven bytes at a time. Loading
    // is announced like any other call: it is something the agent did.
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("load_tools".to_string(), "Getting the tools ready".to_string()),
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
        .expect("run")
        .0;

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
        .0;

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
        .run(&component(), user("What time is it?"), String::new(), options)
        .await;

    // The second request carries the refusal rather than a clock reading, so
    // the model learns its call was dropped instead of acting on a result it
    // never asked for.
    let requests = gateway.requests();
    assert!(requests.len() >= 2, "the turn should continue after refusing");
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

    let runner = runner();
    let reply = runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect("run")
        .0;

    assert_eq!(reply, "2026.");

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
        .run(&component(), user("one"), String::new(), options(&gateway, Some(sink)))
        .await
        .expect("run")
        .0;

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
    let sent = requests[0]["temperature"].as_f64().expect("temperature was not sent");
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
    let (_reply, cost, _held) = runner
        .run(
            &component(),
            user("What day is it?"),
            String::new(),
            options(&gateway, None),
        )
        .await
        .expect("run");

    // Two rounds at 11 prompt tokens each, of which 4 were cached: 7 billed
    // at full rate per round. Folding the cache back in would overstate the
    // bill, which is the mistake the split exists to prevent.
    assert_eq!(
        cost.prompt_tokens, 14,
        "cached tokens are not billed as fresh input"
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
        .run(&component(), user("What day is it?"), String::new(), options)
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

// -- Object storage -----------------------------------------------------------

/// An agent reads and writes only within its own workspace's space.
///
/// The guest is never told which workspace it belongs to, so it cannot name
/// another; and the host resolves every path rather than trusting one, so a
/// component that tries to climb out is refused rather than quietly corrected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_is_scoped_to_the_workspace_and_traversal_is_refused() {
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let ours = scope::Space { workspace_id: Uuid::now_v7(), agent_id: Uuid::now_v7(), session_id: Uuid::now_v7() };
    let theirs = scope::Space { workspace_id: Uuid::now_v7(), agent_id: Uuid::now_v7(), session_id: Uuid::now_v7() };

    // Somebody else's object, which our agent must not be able to reach.
    store
        .write(&scope::resolve(&theirs, "workspace/secrets.txt").unwrap(), 0, b"not yours")
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
        .read(&scope::resolve(&ours, "agent/notes/hello.txt").unwrap(), 0, u32::MAX)
        .await
        .expect("the agent's own file");
    assert_eq!(written, b"written by the agent");

    // And the neighbour's file is untouched and unreachable by name.
    assert!(
        scope::resolve(&ours, "workspace/../../{theirs}/secrets.txt").is_err(),
        "a path climbing out of the space must be refused"
    );
    let theirs_still = store
        .read(&scope::resolve(&theirs, "workspace/secrets.txt").unwrap(), 0, u32::MAX)
        .await
        .expect("still there");
    assert_eq!(theirs_still, b"not yours");
}

/// A path that names no scope is answered with how to write one.
///
/// The one storage error a model will hit most, so the refusal is a
/// correction: it names the three prefixes and shows the path under each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scopeless_path_is_corrected_not_just_refused() {
    use outturn::runtime::storage::MemoryStorage;

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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "write_object".into(),
        arguments: r#"{"path":"workspace/pricing.csv","content":"cheap","action":"Updating prices"}"#.into(),
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
        .run(&component(), user("Update pricing."), String::new(), options)
        .await
        .expect("run");

    assert!(
        store
            .read(&scope::resolve(&space, "workspace/pricing.csv").unwrap(), 0, u32::MAX)
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
    assert!(result.contains("session/"), "the refusal should say where to write instead: {result}");
}

/// Deleting removes the object, rather than leaving an empty one behind.
///
/// The guest has no filesystem, so before `delete_object` existed a model
/// asked to delete reached for the nearest thing it had -- writing nothing
/// over the file -- and then reported a deletion that had not happened, while
/// the panel went on listing a zero-byte file. Nothing listed is the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_removes_the_object_rather_than_emptying_it() {
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
    store.write(&key, 0, b"%PDF-1.4 banana bread").await.expect("seed");

    runner
        .run(&component(), user("Please delete that file now."), String::new(), options)
        .await
        .expect("run");

    assert!(
        store.stat(&key).await.is_err(),
        "the object is still there after a delete"
    );
    let listed = store.list(&scope::root_for(&space, scope::Scope::Session)).await.expect("list");
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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
    store.write(&key, 0, b"not cheap").await.expect("seed");

    runner()
        .run(&component(), user("Delete the price list."), String::new(), options)
        .await
        .expect("run");

    assert!(store.stat(&key).await.is_ok(), "the refused delete went through anyway");
}

/// Deleting nothing is an error, not a quiet success.
///
/// The failure this guards is a model telling someone their file is gone
/// because a tool answered "fine" to a path it never found.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_path_that_names_nothing_says_so() {
    use outturn::runtime::storage::MemoryStorage;

    let gateway = FakeGateway::start(Behavior::ToolThenReply {
        name: "delete_object".into(),
        arguments: r#"{"path":"session/never-existed.md","action":"Deleting the draft"}"#.into(),
        reply: "It was not there.".into(),
    })
    .await;
    let mut options = options(&gateway, None);
    options.storage = Some(Arc::new(MemoryStorage::new()));

    runner()
        .run(&component(), user("Delete the draft."), String::new(), options)
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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
        .write(&scope::resolve(&space, "session/app.log").unwrap(), 0, log.as_bytes())
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

// -- Egress -------------------------------------------------------------------

/// Runs one scripted tool call and returns what the tool produced.
async fn tool_result(arguments: &str, egress: Vec<outturn::runtime::egress::EgressRule>) -> String {
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
/// The gateway's own tests cover the judgement. Here the fake gateway serves
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
    options.egress = vec![outturn::runtime::egress::EgressRule {
        host: "example.com".into(),
        header: None,
        credential_env: None,
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
        vec![outturn::runtime::egress::EgressRule {
            host: "169.254.169.254".into(),
            header: None,
            credential_env: None,
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
        vec![outturn::runtime::egress::EgressRule {
            host: "example.com".into(),
            header: None,
            credential_env: None,
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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
        .write(&scope::resolve(&space, "session/doc.txt").unwrap(), 0, doc.as_bytes())
        .await
        .expect("seed");

    runner
        .run(&component(), user("What does it say?"), String::new(), options)
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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
            0,
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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
        .write(&scope::resolve(&space, "workspace/salaries.csv").unwrap(), 0, b"secret")
        .await
        .expect("seed");

    runner()
        .run(&component(), user("Read the salaries."), String::new(), options)
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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
        .write(&scope::resolve(&space, "workspace/severance.csv").unwrap(), 0, b"x")
        .await
        .expect("seed");
    store
        .write(&scope::resolve(&space, "session/notes.txt").unwrap(), 0, b"y")
        .await
        .expect("seed");

    runner()
        .run(&component(), user("What files are there?"), String::new(), options)
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
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

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
        .write(&scope::resolve(&space, "workspace/severance.csv").unwrap(), 0, b"x")
        .await
        .expect("seed");

    runner()
        .run(&component(), user("List the shared files."), String::new(), options)
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

/// A load that recognised nothing is reported as a failure.
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
            seen.lock().unwrap().push((outcome.content.clone(), outcome.is_error));
        })
    };

    let runner = runner();
    let mut options = options(&gateway, None);
    // This test is about deferral, so nothing is offered outright.
    options.eager_tools = Vec::new();
    options.on_tool_result = Some(on_tool_result);

    runner
        .run(&component(), user("What day is it?"), String::new(), options)
        .await
        .expect("run");

    let results = seen.lock().unwrap().clone();
    let (content, is_error) = results.first().expect("the loader answered").clone();
    assert!(
        is_error,
        "a load that recognised nothing should be an error, got {content:?}"
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
/// Models call tools they were not given: gemma4 emitted `list_objects` with
/// only the loader on offer, carrying the loader's own arguments, and it ran.
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
            seen.lock().unwrap().push((outcome.content.clone(), outcome.is_error));
        })
    };

    let runner = runner();
    let mut options = options(&gateway, None);
    // This test is about deferral, so nothing is offered outright.
    options.eager_tools = Vec::new();
    options.on_tool_result = Some(on_tool_result);

    runner
        .run(&component(), user("What day is it?"), String::new(), options)
        .await
        .expect("run");

    let results = seen.lock().unwrap().clone();
    let (content, is_error) = results.first().expect("the call was answered").clone();
    assert!(is_error, "an unloaded tool should be refused, got {content:?}");
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
            seen.lock().unwrap().push((outcome.content.clone(), outcome.is_error));
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
        .0;

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
        .run(&component(), user("What day is it?"), String::new(), options)
        .await
        .expect("run")
        .0;

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
