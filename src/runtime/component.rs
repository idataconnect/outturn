//! Host side of the agent component interface.
//!
//! Replaces the pointer-and-length imports of the preview 1 sandbox: values
//! cross the boundary as typed records, so there is no manual marshalling and
//! no bounds-checking of guest-supplied offsets.

use std::sync::Arc;

use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "wit",
    world: "agent-world",
    imports: { default: async },
    exports: { default: async },
});

pub use outturn::agent::host::{Completion, CompletionRequest, Message, Usage};

/// Reports text as the model produces it, before the turn finishes.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Marker tying the generated host traits to AgentHost.
struct HostData;

impl wasmtime::component::HasData for HostData {
    type Data<'a> = &'a mut AgentHost;
}

pub struct AgentHost {
    wasi: WasiCtx,
    table: ResourceTable,
    gateway_url: String,
    gateway_token: String,
    default_model: String,
    http: reqwest::Client,
    progress: Option<ProgressSink>,
    session_id: uuid::Uuid,
}

impl WasiView for AgentHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl outturn::agent::host::Host for AgentHost {
    async fn chat(
        &mut self,
        request: CompletionRequest,
    ) -> Result<Completion, String> {
        // The credential is attached here rather than being handed to the
        // guest, so a compromised component can spend this session's allowance
        // but cannot take the token elsewhere.
        let model = request.model.unwrap_or_else(|| self.default_model.clone());

        let body = serde_json::json!({
            "model": model,
            "messages": request.messages.iter().map(|m| serde_json::json!({
                "role": m.role,
                "content": m.content,
            })).collect::<Vec<_>>(),
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
        });

        stream_completion(
            &self.http,
            &self.gateway_url,
            &self.gateway_token,
            body,
            self.progress.as_ref(),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn progress(&mut self, text: String) {
        if let Some(sink) = &self.progress {
            sink(&text);
        }
    }

    async fn log(&mut self, level: String, message: String) {
        match level.as_str() {
            "error" => tracing::error!(session_id = %self.session_id, "guest: {message}"),
            "warn" => tracing::warn!(session_id = %self.session_id, "guest: {message}"),
            _ => tracing::info!(session_id = %self.session_id, "guest: {message}"),
        }
    }
}

/// Consumes the gateway's stream, reporting text as it arrives and returning
/// the whole reply once generation ends.
async fn stream_completion(
    http: &reqwest::Client,
    gateway_url: &str,
    token: &str,
    body: serde_json::Value,
    progress: Option<&ProgressSink>,
) -> anyhow::Result<Completion> {
    use futures::StreamExt;

    let response = http
        .post(format!("{gateway_url}/v1/chat/completions/stream"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        anyhow::bail!("gateway returned {status}: {detail}");
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut content = String::new();
    let mut finish_reason = None;
    let mut usage = None;

    while let Some(bytes) = stream.next().await {
        buffer.push_str(std::str::from_utf8(&bytes?)?);

        // A chunk may span reads, so only whole lines are parsed.
        while let Some(index) = buffer.find('\n') {
            let line = buffer[..index].trim().to_string();
            buffer.drain(..=index);
            if line.is_empty() {
                continue;
            }

            let chunk: serde_json::Value = match serde_json::from_str(&line) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "malformed chunk from gateway");
                    continue;
                }
            };

            if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str()
                && !text.is_empty()
            {
                content.push_str(text);
                if let Some(sink) = progress {
                    sink(text);
                }
            }
            if let Some(reason) = chunk["choices"][0]["finish_reason"].as_str() {
                finish_reason = Some(reason.to_string());
            }
            if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
                usage = Some(Usage {
                    prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                    completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                });
            }
        }
    }

    Ok(Completion {
        content,
        finish_reason,
        usage,
    })
}

pub struct AgentRunner {
    engine: Engine,
}

pub struct RunOptions {
    pub session_id: uuid::Uuid,
    pub gateway_url: String,
    pub gateway_token: String,
    pub default_model: String,
    pub progress: Option<ProgressSink>,
    pub fuel: u64,
}

impl AgentRunner {
    pub fn new() -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.async_support(true);
        // Fuel bounds a runaway guest; without it a loop in a component would
        // occupy a worker indefinitely.
        config.consume_fuel(true);
        Ok(Self {
            engine: Engine::new(&config)?,
        })
    }

    pub async fn run(
        &self,
        component_bytes: &[u8],
        conversation: Vec<Message>,
        system_prompt: String,
        options: RunOptions,
    ) -> anyhow::Result<String> {
        let component = Component::new(&self.engine, component_bytes)?;

        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        // The generated linker needs a HasData marker naming which type the
        // host implementations live on.
        AgentWorld::add_to_linker::<_, HostData>(&mut linker, |state: &mut AgentHost| state)?;

        // No preopened directories, no environment, no network: everything the
        // guest can reach is an explicit import.
        let host = AgentHost {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            gateway_url: options.gateway_url,
            gateway_token: options.gateway_token,
            default_model: options.default_model,
            http: reqwest::Client::new(),
            progress: options.progress,
            session_id: options.session_id,
        };

        let mut store = Store::new(&self.engine, host);
        store.set_fuel(options.fuel)?;

        let instance = AgentWorld::instantiate_async(&mut store, &component, &linker).await?;

        instance
            .outturn_agent_agent()
            .call_run(&mut store, &conversation, &system_prompt)
            .await?
            .map_err(|e| anyhow::anyhow!("guest returned an error: {e}"))
    }
}
