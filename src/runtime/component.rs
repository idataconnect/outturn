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

pub use outturn::agent::host::{
    Clock, Completion, CompletionRequest, Message, ToolActivity, ToolCall, ToolDefinition, Usage,
};

/// Reports text as the model produces it, before the turn finishes.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Reports a tool call as the guest starts it.
pub type ToolSink = Arc<dyn Fn(&ToolActivity) + Send + Sync>;

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
    /// IANA zone of the user this turn belongs to. None when the client did
    /// not say, in which case the clock answers in UTC rather than guessing.
    timezone: Option<chrono_tz::Tz>,
    /// What this turn's work is for, so the gateway can route it. Pinned by
    /// the runtime and resolved there: the agent asks for a completion, not
    /// for a particular endpoint.
    traffic_type: String,
    /// How much the model should deliberate, when the provider offers the
    /// choice. Attached here rather than in the guest: which models think, and
    /// what the knob is called, is a provider detail an agent should not have
    /// to know.
    reasoning_effort: Option<String>,
    on_tool: Option<ToolSink>,
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

        let messages = request
            .messages
            .iter()
            .map(|m| {
                let mut value = serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                });
                if !m.tool_calls.is_empty() {
                    value["tool_calls"] = serde_json::json!(
                        m.tool_calls
                            .iter()
                            .map(|c| serde_json::json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.arguments },
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                if let Some(id) = &m.tool_call_id {
                    value["tool_call_id"] = serde_json::json!(id);
                }
                value
            })
            .collect::<Vec<_>>();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
        });

        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = serde_json::json!(effort);
        }

        // Omitted rather than sent empty: offering no tools is the common
        // case, and some providers reject an empty array.
        if !request.tools.is_empty() {
            body["tools"] = serde_json::json!(
                request
                    .tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            // Schemas cross as text because WIT has no JSON
                            // type. Unparseable ones degrade to an empty
                            // object, so a malformed schema costs the tool its
                            // arguments rather than failing the whole turn.
                            "parameters": serde_json::from_str::<serde_json::Value>(&t.parameters)
                                .unwrap_or_else(|_| serde_json::json!({"type": "object"})),
                        },
                    }))
                    .collect::<Vec<_>>()
            );
        }

        stream_completion(
            &self.http,
            &self.gateway_url,
            &self.gateway_token,
            &self.traffic_type,
            body,
            self.progress.as_ref(),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn tool_started(&mut self, activity: ToolActivity) {
        tracing::info!(
            session_id = %self.session_id,
            tool = %activity.name,
            "guest is running a tool"
        );
        if let Some(sink) = &self.on_tool {
            sink(&activity);
        }
    }

    async fn current_time(&mut self) -> Clock {
        // The zone belongs to the user, not to this machine: the runtime runs
        // in a container that is almost certainly UTC, so answering from its
        // own locale would be confidently wrong for everyone.
        use chrono::SecondsFormat;

        match self.timezone {
            Some(tz) => {
                let now = chrono::Utc::now().with_timezone(&tz);
                Clock {
                    now: now.to_rfc3339_opts(SecondsFormat::Secs, false),
                    weekday: now.format("%A").to_string(),
                    timezone: tz.name().to_string(),
                    abbreviation: now.format("%Z").to_string(),
                }
            }
            None => {
                let now = chrono::Utc::now();
                Clock {
                    now: now.to_rfc3339_opts(SecondsFormat::Secs, true),
                    weekday: now.format("%A").to_string(),
                    timezone: String::new(),
                    abbreviation: "UTC".to_string(),
                }
            }
        }
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
    traffic_type: &str,
    body: serde_json::Value,
    progress: Option<&ProgressSink>,
) -> anyhow::Result<Completion> {
    use futures::StreamExt;

    let response = http
        .post(format!("{gateway_url}/v1/chat/completions/stream"))
        .bearer_auth(token)
        .header("x-outturn-traffic", traffic_type)
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
    // Tool calls arrive in fragments keyed by index, and the arguments are a
    // JSON string spread across chunks. Kept sparse by index rather than
    // pushed, since a provider is free to interleave two calls.
    let mut partial_calls: std::collections::BTreeMap<u32, PartialToolCall> =
        std::collections::BTreeMap::new();

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
            if let Some(calls) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"].as_u64().unwrap_or(0) as u32;
                    let entry = partial_calls.entry(index).or_default();
                    if let Some(id) = call["id"].as_str() {
                        entry.id = id.to_string();
                    }
                    if let Some(name) = call["function"]["name"].as_str() {
                        entry.name.push_str(name);
                    }
                    if let Some(args) = call["function"]["arguments"].as_str() {
                        entry.arguments.push_str(args);
                    }
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
        tool_calls: partial_calls
            .into_values()
            // A call with no name is a fragment of something that never
            // arrived; passing it on would have the guest dispatch on "".
            .filter(|c| !c.name.is_empty())
            .map(|c| ToolCall {
                id: c.id,
                name: c.name,
                arguments: c.arguments,
            })
            .collect(),
        finish_reason,
        usage,
    })
}

/// One tool call being assembled from stream fragments.
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
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
    pub on_tool: Option<ToolSink>,
    pub fuel: u64,
    /// IANA zone of the user this turn belongs to, as the client reported it.
    /// Unrecognised or absent means the clock answers in UTC.
    pub timezone: Option<String>,
    /// Passed to providers that support it; ignored by those that do not.
    pub reasoning_effort: Option<String>,
    /// Names the class of traffic, which the gateway resolves to a route.
    pub traffic_type: String,
    /// How long the gateway's stream may go silent before the turn is
    /// abandoned. A parameter so a test can prove it fires.
    pub idle_timeout: std::time::Duration,
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
            http: crate::http_client::streaming_client(options.idle_timeout),
            progress: options.progress,
            on_tool: options.on_tool,
            session_id: options.session_id,
            // Parsed here so a bad zone from a client degrades to UTC once,
            // rather than on every call the guest makes.
            reasoning_effort: options.reasoning_effort,
            traffic_type: options.traffic_type,
            timezone: options.timezone.as_deref().and_then(|tz| {
                tz.parse::<chrono_tz::Tz>()
                    .inspect_err(|_| tracing::warn!(timezone = tz, "unknown timezone, using UTC"))
                    .ok()
            }),
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
