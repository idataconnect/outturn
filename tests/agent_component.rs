//! Runs the default agent component against a live gateway.
//!
//! Built only under the integration-tests feature. Needs GATEWAY_URL and a
//! reachable model:
//!
//!   kubectl port-forward svc/outturn-gateway 18091:8081
//!   GATEWAY_URL=http://localhost:18091 \\
//!     cargo test --features integration-tests --test agent_component

use std::sync::{Arc, Mutex};

use outturn::auth::{Role, TokenMinter};
use outturn::runtime::component::{AgentRunner, RunOptions};
use uuid::Uuid;

const DEV_SECRET: &str = "993c3d8e41668abaa0151de741215ef5bf5022b62bdb8468122df597c70d5887";

fn dev_token(session_id: Uuid, tenant_id: Uuid) -> String {
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&DEV_SECRET[i * 2..i * 2 + 2], 16).unwrap();
    }
    let minter = TokenMinter::new(&bytes).expect("minter");
    minter
        .mint(session_id, tenant_id, &[Role::Operator])
        .expect("mint")
}

#[tokio::test(flavor = "multi_thread")]
async fn component_runs_a_turn_and_streams_progress() {
    // Panics rather than skipping: this suite only builds under the
    // integration-tests feature, so a missing gateway is a misconfiguration.
    let gateway_url = std::env::var("GATEWAY_URL").expect(
        "GATEWAY_URL must be set, e.g. http://localhost:18091 with \
         `kubectl port-forward svc/outturn-gateway 18091:8081` running",
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
    let tenant_id = Uuid::now_v7();

    let reply = runner
        .run(
            &component,
            vec![outturn::runtime::component::Message {
                role: "user".into(),
                content: "Count from one to twenty in words, separated by commas.".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
            "You are concise but complete.".into(),
            RunOptions {
                session_id,
                gateway_url,
                gateway_token: dev_token(session_id, tenant_id),
                default_model: std::env::var("OUTTURN_DEFAULT_MODEL")
                    .unwrap_or_else(|_| "llama3.1".into()),
                progress: Some(sink),
                on_tool: None,
                timezone: None,
                reasoning_effort: None,
                fuel: 10_000_000_000,
            },
        )
        .await
        .expect("component run");

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
