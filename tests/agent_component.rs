//! Runs the default agent component against a live gateway and a real model.
//!
//! Behind `live-providers`, not `integration-tests`, because the gateway this
//! reaches may be pointed at a provider that charges: the flag is the consent,
//! and having services running is not the same sentence as being willing to
//! pay for a model to write out the numbers one to twenty.
//!
//! What it adds over `agent_component_fake`, which asserts the same three
//! things in under a second, is that the gateway is a real process and the
//! model is a real one -- which is how the gemma4-against-llama3.1 streaming
//! difference was found. Worth running before a release; not worth running on
//! every push.
//!
//!   kubectl port-forward svc/outturn-gateway 18091:8081
//!   GATEWAY_URL=http://localhost:18091 \\
//!     cargo test --features live-providers --test agent_component

use std::sync::{Arc, Mutex};

use outturn::auth::TokenMinter;
use outturn::runtime::component::{AgentRunner, RunOptions};
use uuid::Uuid;

/// The key the gateway under test trusts: this clone's, as
/// `scripts/dev-secrets.sh` wrote it, unless the environment names another.
/// A key written here would be one every clone shares, and one no local
/// gateway accepts since each clone generates its own.
fn token_secret() -> String {
    if let Ok(secret) = std::env::var("OUTTURN_TOKEN_SECRET") {
        return secret;
    }
    let file = std::fs::read_to_string("k8s/overlays/local/dev-secrets.env").expect(
        "OUTTURN_TOKEN_SECRET unset and no k8s/overlays/local/dev-secrets.env; \
         run scripts/dev-secrets.sh",
    );
    file.lines()
        .find_map(|line| line.strip_prefix("OUTTURN_TOKEN_SECRET="))
        .expect("no OUTTURN_TOKEN_SECRET in dev-secrets.env")
        .trim()
        .to_string()
}

fn dev_token(session_id: Uuid, workspace_id: Uuid) -> String {
    let secret = token_secret();
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&secret[i * 2..i * 2 + 2], 16).expect("hex secret");
    }
    let minter = TokenMinter::new(&bytes).expect("minter");
    // No egress rules are exercised by this suite, so the empty commitment is
    // what a real turn for a workspace with none would carry too.
    minter
        .mint_turn(
            session_id,
            workspace_id,
            outturn::egress::commit::empty_root(),
        )
        .expect("mint")
}

#[tokio::test(flavor = "multi_thread")]
async fn component_runs_a_turn_and_streams_progress() {
    // Panics rather than skipping: this suite only builds under the
    // integration-tests feature, so a missing gateway is a misconfiguration.
    let gateway_url = std::env::var("GATEWAY_URL").expect(
        "GATEWAY_URL must be set, e.g. http://localhost:18081, which \
         `skaffold dev` forwards to the gateway",
    );

    let component = std::fs::read("assets/agent_default.wasm").expect("component");
    let runner = AgentRunner::new().expect("runner");

    // Progress arrives while the model generates, which is what will drive
    // tokens to the browser.
    let deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let deltas = Arc::clone(&deltas);
        Arc::new(move |text: &str| {
            deltas.lock().unwrap().push(text.to_string());
        })
    };

    let session_id = Uuid::now_v7();
    let workspace_id = Uuid::now_v7();

    let reply = runner
        .run(
            &component,
            vec![outturn::runtime::component::Message {
                role: "user".into(),
                parts: vec![outturn::runtime::component::ContentPart::Text(
                    "Count from one to twenty in words, separated by commas.".into(),
                )],
                tool_call_id: None,
            }],
            "You are concise but complete.".into(),
            RunOptions {
                session_id,
                gateway_url,
                gateway_token: dev_token(session_id, workspace_id),
                default_model: std::env::var("OUTTURN_DEFAULT_MODEL")
                    .unwrap_or_else(|_| "qwen3.5".into()),
                progress: Some(sink),
                on_tool: None,
                on_tool_result: None,
                on_usage: None,
                on_write: None,
                on_absorbed: None,
                storage: None,
                workspace_id,
                agent_id: Uuid::now_v7(),
                write_scopes: vec!["session".into()],
                read_scopes: vec!["session".into()],
                timezone: None,
                reasoning_effort: None,
                temperature: None,
                traffic_type: "assistant".into(),
                max_tool_rounds: 100,
                // As the runtime runs a turn: every tool behind the loader.
                eager_tools: Vec::new(),
                reply_id: Uuid::now_v7(),
                idle_timeout: outturn::http_client::IDLE_TIMEOUT,
                egress: Vec::new(),
                fuel: 10_000_000_000,
            },
        )
        .await
        .expect("component run")
        .0;

    assert!(!reply.is_empty(), "the component returned nothing");

    let seen = deltas.lock().unwrap();
    // A reply of this length spans many tokens, so a single delta would mean
    // the host buffered the whole response rather than streaming it.
    assert!(
        seen.len() > 3,
        "expected several progress deltas, got {}: the host is not streaming",
        seen.len()
    );

    // What the guest returned must equal what streamed, or the browser would
    // see one thing during generation and another when it completed.
    assert_eq!(
        seen.concat(),
        reply,
        "streamed text must match the final reply exactly"
    );

    println!("reply: {reply}");
    println!("deltas: {}", seen.len());
}
