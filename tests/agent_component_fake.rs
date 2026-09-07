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

fn options(gateway: &FakeGateway, progress: Option<Arc<dyn Fn(&str) + Send + Sync>>) -> RunOptions {
    RunOptions {
        session_id: Uuid::now_v7(),
        gateway_url: gateway.url.clone(),
        gateway_token: "test-token".into(),
        default_model: "fake".into(),
        progress,
        on_tool: None,
        on_tool_result: None,
        storage: None,
        tenant_id: Uuid::now_v7(),
        timezone: None,
        reasoning_effort: None,
        traffic_type: "assistant".into(),
        max_tool_rounds: 100,
        reply_id: Uuid::now_v7(),
        // Production waits five minutes; a test cannot.
        idle_timeout: std::time::Duration::from_secs(2),
        // Nothing reachable unless a test says so, which is the default a
        // tenant gets.
        egress: Vec::new(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_deltas_and_returns_the_whole_reply() {
    let gateway = FakeGateway::start(Behaviour::Reply("one two three four".into())).await;
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
    let gateway = FakeGateway::start(Behaviour::Reply("ok".into())).await;
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
    assert_eq!(messages[0]["content"], "you are a lighthouse");
    assert_eq!(messages[1]["role"], "user");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_failure_surfaces_as_an_error() {
    let gateway = FakeGateway::start(Behaviour::Status(
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
    let gateway = FakeGateway::start(Behaviour::TruncateAfter {
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
    let gateway = FakeGateway::start(Behaviour::Reply("unused".into())).await;
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_a_tool_and_answers_with_its_result() {
    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
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

    // The guest announced the call, with the model's own reason attached --
    // reassembled from arguments that arrived seven bytes at a time.
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            "get_current_time".to_string(),
            "Checking today's date".to_string()
        )]
    );

    let requests = gateway.requests();
    assert_eq!(requests.len(), 2, "one call to ask, one to answer");

    // The tool was among those offered on the first request. Which position
    // it holds is not meaningful, and asserting one made this break the
    // moment another tool was added.
    let offered: Vec<&str> = requests[0]["tools"]
        .as_array()
        .expect("tools were offered")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        offered.contains(&"get_current_time"),
        "the clock should be on offer, got {offered:?}"
    );

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
        !echoed.contains("action"),
        "the label is written for the user and must not be resent to the model, got {echoed:?}"
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
    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
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
    let gateway = FakeGateway::start(Behaviour::Hang).await;
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
    let gateway = FakeGateway::start(Behaviour::AlwaysToolCall {
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
/// A component is deployed by a tenant, so a bound that lives only in guest
/// code is a suggestion. The host counts the calls it makes and refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_refuses_past_the_limit_whatever_the_guest_intends() {
    let gateway = FakeGateway::start(Behaviour::AlwaysToolCall {
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
    let gateway = FakeGateway::start(Behaviour::TruncatedToolCall {
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
    let gateway = FakeGateway::start(Behaviour::ToolThenSteer {
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
    let gateway = FakeGateway::start(Behaviour::TextThenSteer {
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

// -- Accounting ---------------------------------------------------------------

/// What a turn spent is counted by the host, across every round.
///
/// The guest never sees these numbers and cannot report them: asking a
/// component deployed by a tenant to declare its own spend is asking the party
/// being billed to write the invoice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_reports_what_it_spent() {
    // Two rounds: a tool call, then the answer. Each reports usage, so a
    // turn that only counted the last one would come up short.
    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
        name: "get_current_time".into(),
        arguments: r#"{"action":"Checking the clock"}"#.into(),
        reply: "Tuesday.".into(),
    })
    .await;

    let runner = runner();
    let (_reply, cost) = runner
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
    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
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

/// An agent reads and writes only within its own tenant's space.
///
/// The guest is never told which tenant it belongs to, so it cannot name
/// another; and the host resolves every path rather than trusting one, so a
/// component that tries to climb out is refused rather than quietly corrected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_is_scoped_to_the_tenant_and_traversal_is_refused() {
    use outturn::runtime::storage::{MemoryStorage, StorageBackend, scope};

    let store = Arc::new(MemoryStorage::new());
    let ours = Uuid::now_v7();
    let theirs = Uuid::now_v7();

    // Somebody else's object, which our agent must not be able to reach.
    store
        .write(&scope::resolve(theirs, "secrets.txt").unwrap(), 0, b"not yours")
        .await
        .expect("seed");

    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
        name: "write_object".into(),
        arguments: r#"{"path":"notes/hello.txt","content":"written by the agent","action":"Saving a note"}"#.into(),
        reply: "Saved.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.tenant_id = ours;

    runner
        .run(&component(), user("Write a note."), String::new(), options)
        .await
        .expect("run");

    // It landed under our tenant, not at the path the guest named.
    let written = store
        .read(&scope::resolve(ours, "notes/hello.txt").unwrap(), 0, u32::MAX)
        .await
        .expect("the agent's own file");
    assert_eq!(written, b"written by the agent");

    // And the neighbour's file is untouched and unreachable by name.
    assert!(
        scope::resolve(ours, "../{theirs}/secrets.txt").is_err(),
        "a path climbing out of the tenant root must be refused"
    );
    let theirs_still = store
        .read(&scope::resolve(theirs, "secrets.txt").unwrap(), 0, u32::MAX)
        .await
        .expect("still there");
    assert_eq!(theirs_still, b"not yours");
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
    let tenant = Uuid::now_v7();

    // Distinctive first and last lines, with plenty of filler between.
    let mut log = String::from("FIRST LINE: service starting\n");
    for i in 0..5000 {
        log.push_str(&format!("line {i} of unremarkable middle\n"));
    }
    log.push_str("LAST LINE: everything caught fire\n");
    store
        .write(&scope::resolve(tenant, "app.log").unwrap(), 0, log.as_bytes())
        .await
        .expect("seed");

    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
        name: "read_object".into(),
        arguments: r#"{"path":"app.log","action":"Reading the log"}"#.into(),
        reply: "It caught fire.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.tenant_id = tenant;

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
    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
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

/// An agent reaches nothing until a tenant says otherwise.
///
/// The default matters more than any rule: a tenant who has not thought about
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

/// Allowing a name does not allow what the name resolves to.
///
/// This is the check a tenant cannot waive. `localhost` is a host like any
/// other as far as a rule is concerned, and a tenant could name it by accident
/// or be talked into it -- what stops the request is that the address it
/// resolves to is inside the network this runs in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_the_tenant_allowed_still_cannot_reach_the_cluster() {
    let result = tool_result(
        r#"{"url":"http://localhost:5432/","action":"Looking something up"}"#,
        vec![outturn::runtime::egress::EgressRule {
            host: "localhost".into(),
            header: None,
            credential_env: None,
        }],
    )
    .await;

    assert!(
        result.contains("cannot be reached from an agent"),
        "an allowed name reached inside the cluster: {result}"
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

/// A guest cannot aim the tenant's credential somewhere else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_cannot_set_the_headers_the_host_owns() {
    let result = tool_result(
        r#"{"url":"https://example.com/","headers":{"Authorization":"Bearer stolen"},"action":"Looking something up"}"#,
        vec![outturn::runtime::egress::EgressRule {
            host: "example.com".into(),
            header: Some("authorization".into()),
            credential_env: Some("EXAMPLE_KEY".into()),
        }],
    )
    .await;

    assert!(
        result.contains("set by the platform"),
        "a guest set its own authorization header: {result}"
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
    let tenant = Uuid::now_v7();

    // Three-byte characters throughout, so wherever the tail begins it has a
    // good chance of landing inside one. Padded with a single ASCII character
    // per line so the offsets do not all align to three.
    let mut doc = String::from("FIRST LINE: 見出し\n");
    for i in 0..5000 {
        doc.push_str(&format!("行 {i} — 中身の行がここにあります\n"));
    }
    doc.push_str("LAST LINE: 終わり\n");
    store
        .write(&scope::resolve(tenant, "doc.txt").unwrap(), 0, doc.as_bytes())
        .await
        .expect("seed");

    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
        name: "read_object".into(),
        arguments: r#"{"path":"doc.txt","action":"Reading the document"}"#.into(),
        reply: "Read it.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.tenant_id = tenant;

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
    let tenant = Uuid::now_v7();

    // One line, no newlines, well past the byte ceiling.
    let minified = "a".repeat(200 * 1024);
    store
        .write(
            &scope::resolve(tenant, "bundle.min.js").unwrap(),
            0,
            minified.as_bytes(),
        )
        .await
        .expect("seed");

    let gateway = FakeGateway::start(Behaviour::ToolThenReply {
        name: "read_object".into(),
        arguments: r#"{"path":"bundle.min.js","action":"Reading the bundle"}"#.into(),
        reply: "Had a look.".into(),
    })
    .await;

    let runner = runner();
    let mut options = options(&gateway, None);
    options.storage = Some(store.clone());
    options.tenant_id = tenant;

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
