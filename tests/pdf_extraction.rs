use std::sync::Arc;

use outturn::runtime::sandbox::{Sandbox, SandboxConfig, SecurityTier};
use outturn::runtime::storage::MemoryStorage;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn pdf_extract_text_via_wasm() {
    let storage = Arc::new(MemoryStorage::new());

    let pdf_bytes = std::fs::read("tests/fixtures/hello.pdf").expect("test PDF not found");
    storage
        .write_file("test.pdf", &pdf_bytes)
        .await;

    let wasm_bytes = std::fs::read("tests/fixtures/test_guest.wasm")
        .expect("guest WASM not found — build it from ../outturn-test-guest");

    let config = SandboxConfig {
        session_id: Uuid::now_v7(),
        tenant_id: Uuid::now_v7(),
        security_tier: SecurityTier::Standard,
        gateway_token: "test-token".into(),
        gateway_url: "http://localhost:8081".into(),
        memory_limit_bytes: 64 * 1024 * 1024,
        fuel_limit: 1_000_000_000,
    };

    let sandbox = Sandbox::new(config, storage.clone()).expect("failed to create sandbox");
    sandbox.run(&wasm_bytes).await.expect("sandbox execution failed");

    let output = storage.read_file("output.txt").await.expect("output.txt not found");
    let text = String::from_utf8(output).expect("output is not utf-8");

    assert!(
        text.contains("Hello World"),
        "expected 'Hello World' in extracted text, got: {text:?}"
    );
}
