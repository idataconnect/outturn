//! Host side of the agent component interface.
//!
//! Replaces the pointer-and-length imports of the preview 1 sandbox: values
//! cross the boundary as typed records, so there is no manual marshalling and
//! no bounds-checking of guest-supplied offsets.

use std::sync::{Arc, Mutex};

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
    Arrival, Clock, Completion, CompletionRequest, Limits, Message, ObjectInfo, ToolActivity,
    ToolCall, ToolDefinition, ToolOutcome, Usage,
};

/// Reports text as the model produces it, before the turn finishes.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Reports a tool call as the guest starts it.
pub type ToolSink = Arc<dyn Fn(&ToolActivity) + Send + Sync>;

/// Reports what a tool produced, for the reader rather than the model.
pub type ToolResultSink = Arc<dyn Fn(&ToolOutcome) + Send + Sync>;

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
    on_tool_result: Option<ToolResultSink>,
    /// Zero means unbounded.
    max_tool_rounds: u32,
    /// Model calls made so far this turn, counted host-side so a guest that
    /// ignores `limits` still cannot exceed them.
    rounds_used: u32,
    /// What the user has said since this turn began, as reported by the
    /// gateway on the responses it was already sending. Drained when the guest
    /// asks, so each message is injected once.
    arrivals: Vec<Arrival>,
    /// The reply this turn is writing. Sent to the gateway so it can record
    /// which reply absorbed a message it handed over.
    reply_id: uuid::Uuid,
    /// What this turn has spent, summed across every round.
    ///
    /// Counted by the host rather than reported by the guest, for the same
    /// reason the round limit is enforced here: a component is deployed by a
    /// tenant, and asking it to declare its own spend is asking the party
    /// being billed to write the invoice.
    spent: Usage,
    /// Which endpoint served this turn, as the gateway reported it.
    served_by: Option<String>,
    /// Object storage, and the tenant whose corner of it this turn may touch.
    /// Absent leaves the guest with no storage at all rather than with
    /// somebody else's.
    storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
    tenant_id: uuid::Uuid,
}

impl AgentHost {
    /// Resolves a guest path, or refuses it.
    ///
    /// The check is here rather than in the guest for the same reason the
    /// credential is: a component is deployed by a tenant, and a boundary it
    /// enforces on itself is not a boundary.
    fn object_at(
        &self,
        path: &str,
    ) -> Result<(Arc<dyn crate::runtime::storage::StorageBackend>, String), String> {
        let Some(storage) = self.storage.clone() else {
            return Err("no object storage is configured".to_string());
        };
        let resolved = crate::runtime::storage::scope::resolve(self.tenant_id, path)
            .map_err(|_| format!("path is not allowed: {path}"))?;
        Ok((storage, resolved))
    }
}

/// What a turn cost, and who served it.
#[derive(Debug, Clone, Default)]
pub struct TurnCost {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub reasoning_tokens: u32,
    pub provider: Option<String>,
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
        // Counted before the call, not after: the limit is on what may be
        // spent, and a guest that ignores what `limits` told it is refused
        // here rather than politely asked again.
        self.rounds_used += 1;
        if self.max_tool_rounds > 0 && self.rounds_used > self.max_tool_rounds {
            tracing::warn!(
                session_id = %self.session_id,
                limit = self.max_tool_rounds,
                "guest exceeded its round limit"
            );
            return Err(format!(
                "round limit reached: this turn may call the model {} times",
                self.max_tool_rounds
            ));
        }

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

        let (completion, arrivals, served_by) = stream_completion(
            &self.http,
            &self.gateway_url,
            &self.gateway_token,
            &self.traffic_type,
            &self.reply_id,
            body,
            self.progress.as_ref(),
        )
        .await
        .map_err(|e| e.to_string())?;

        // Buffered rather than delivered: the guest asks at a boundary it
        // chooses, which is the only point where injecting a message does not
        // corrupt a round already in flight.
        self.arrivals.extend(arrivals);

        // Summed across rounds: a turn's cost is every call it made, not the
        // last one. A provider that reports nothing simply adds nothing.
        if let Some(usage) = &completion.usage {
            self.spent.prompt_tokens += usage.prompt_tokens;
            self.spent.completion_tokens += usage.completion_tokens;
            self.spent.cache_read_tokens += usage.cache_read_tokens;
            self.spent.cache_write_tokens += usage.cache_write_tokens;
            self.spent.reasoning_tokens += usage.reasoning_tokens;
        }
        if served_by.is_some() {
            self.served_by = served_by;
        }

        Ok(completion)
    }

    async fn read_object(
        &mut self,
        path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        let (storage, resolved) = self.object_at(&path)?;
        storage
            .read(&resolved, offset, len)
            .await
            .map_err(|e| e.to_string())
    }

    async fn stat_object(&mut self, path: String) -> Result<ObjectInfo, String> {
        let (storage, resolved) = self.object_at(&path)?;
        let found = storage.stat(&resolved).await.map_err(|e| e.to_string())?;
        Ok(ObjectInfo {
            // Handed back as the guest named it, not as it is stored.
            path,
            size: found.size,
        })
    }

    async fn write_object(&mut self, path: String, data: Vec<u8>) -> Result<u64, String> {
        let (storage, resolved) = self.object_at(&path)?;
        storage
            .write(&resolved, 0, &data)
            .await
            .map_err(|e| e.to_string())
    }

    async fn list_objects(&mut self, prefix: String) -> Result<Vec<ObjectInfo>, String> {
        let Some(storage) = self.storage.clone() else {
            return Err("no object storage is configured".to_string());
        };
        // An empty prefix means "everything I have", which resolve would
        // reject as a path -- so the root is built directly rather than
        // through it.
        let root = crate::runtime::storage::scope::root_for(self.tenant_id);
        let resolved = if prefix.trim().is_empty() {
            root
        } else {
            crate::runtime::storage::scope::resolve(self.tenant_id, &prefix)
                .map_err(|e| e.to_string())?
        };

        let found = storage.list(&resolved).await.map_err(|e| e.to_string())?;
        Ok(found
            .iter()
            .filter(|f| !f.is_dir)
            .map(|f| ObjectInfo {
                path: crate::runtime::storage::scope::strip_root(self.tenant_id, &f.path),
                size: f.size,
            })
            .collect())
    }

    async fn tool_finished(&mut self, outcome: ToolOutcome) {
        if let Some(sink) = &self.on_tool_result {
            sink(&outcome);
        }
    }

    async fn pending_input(&mut self) -> Vec<Arrival> {
        std::mem::take(&mut self.arrivals)
    }

    async fn current_limits(&mut self) -> Limits {
        Limits {
            max_tool_rounds: self.max_tool_rounds,
        }
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
    reply_id: &uuid::Uuid,
    body: serde_json::Value,
    progress: Option<&ProgressSink>,
) -> anyhow::Result<(Completion, Vec<Arrival>, Option<String>)> {
    use futures::StreamExt;

    let response = http
        .post(format!("{gateway_url}/v1/chat/completions/stream"))
        .bearer_auth(token)
        .header("x-outturn-traffic", traffic_type)
        // Named so the gateway can record which reply took a message it hands
        // back, rather than only that one was taken.
        .header("x-outturn-reply", reply_id.to_string())
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        anyhow::bail!("gateway returned {status}: {detail}");
    }

    // Named by the gateway, so spend attaches to the endpoint that billed
    // for it rather than to whichever one was configured first.
    let served_by = response
        .headers()
        .get("x-outturn-provider")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

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
    let mut arrivals: Vec<Arrival> = Vec::new();

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
            // The gateway appends its own line after the provider's chunks,
            // carrying anything the user said while this call was in flight.
            // It rides the response rather than needing a channel of its own,
            // because the runtime holds no credentials and no database.
            if let Some(pending) = chunk["outturn"]["pending"].as_array() {
                for message in pending {
                    arrivals.push(Arrival {
                        content: message["content"].as_str().unwrap_or_default().to_string(),
                        delivery: message["delivery"].as_str().unwrap_or("steer").to_string(),
                    });
                }
                continue;
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
                let cached = u["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0) as u32;
                let prompt = u["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                usage = Some(Usage {
                    // This protocol counts cached tokens inside the prompt
                    // total, so they are taken back out: "prompt tokens" here
                    // means the ones billed at full rate, whatever a given
                    // provider chooses to fold together.
                    prompt_tokens: prompt.saturating_sub(cached),
                    completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                    cache_read_tokens: cached,
                    cache_write_tokens: u["prompt_tokens_details"]["cache_creation_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32,
                    reasoning_tokens: u["completion_tokens_details"]["reasoning_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32,
                });
            }
        }
    }

    Ok((Completion {
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
    }, arrivals, served_by))
}

/// One tool call being assembled from stream fragments.
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Compiled components to keep, keyed by the bytes they came from.
///
/// A cap because the map is keyed by tenant-supplied content: without one,
/// uploading a component is a way to make the runtime allocate, over and over,
/// with nothing to reclaim it. Small, because in practice a node serves a
/// handful of distinct agents and a miss costs one compile, not a failure.
const COMPILED_CACHE_ENTRIES: usize = 32;

/// How long an unused compiled component is kept.
///
/// Long enough that an idle tenant does not pay a compile on every message,
/// short enough that a redeployed agent's previous build is gone within the
/// hour rather than at the next pod recycle.
const COMPILED_CACHE_IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Compiling a component is pure: the same bytes always produce the same
/// module, so the work only has to happen once per distinct guest rather than
/// once per turn.
///
/// Keyed by a hash of the bytes rather than by agent id, because a compiled
/// component is only valid for the bytes it came from -- an agent that is
/// redeployed with changes must not be served its previous build, and one
/// redeployed unchanged should be.
///
/// Most-recently-used last. Thirty-two entries is short enough that scanning
/// costs less than the bookkeeping to avoid it.
struct CompiledCache<T = Component> {
    entries: Mutex<Vec<CompiledEntry<T>>>,
}

struct CompiledEntry<T> {
    key: [u8; 32],
    component: T,
    last_used: std::time::Instant,
}

impl<T: Clone> CompiledCache<T> {
    fn new() -> Self {
        Self { entries: Mutex::new(Vec::new()) }
    }

    fn get(&self, key: &[u8; 32]) -> Option<T> {
        let mut entries = self.entries.lock().ok()?;
        Self::drop_idle(&mut entries);
        let found = entries.iter().position(|e| &e.key == key)?;
        // Move to the back so eviction takes the least recently used.
        let mut entry = entries.remove(found);
        entry.last_used = std::time::Instant::now();
        let component = entry.component.clone();
        entries.push(entry);
        Some(component)
    }

    fn insert(&self, key: [u8; 32], component: T) {
        let Ok(mut entries) = self.entries.lock() else {
            // A poisoned cache is a lost optimisation, not a lost turn.
            return;
        };
        Self::drop_idle(&mut entries);
        if entries.iter().any(|e| e.key == key) {
            return;
        }
        entries.push(CompiledEntry {
            key,
            component,
            last_used: std::time::Instant::now(),
        });
        if entries.len() > COMPILED_CACHE_ENTRIES {
            entries.remove(0);
        }
    }

    /// Forgets what has not been asked for lately.
    ///
    /// The size cap alone would not do this. A node serving one agent holds
    /// one entry, so a version that has been superseded is never pushed out --
    /// it just sits there, resident for the life of the process, holding the
    /// compiled form of code nobody runs any more. Redeploys are exactly when
    /// that happens, and the old bytes are never asked for again.
    ///
    /// Swept on access rather than by a timer: a cache nobody is using is not
    /// costing anything to sweep, and one that is busy sweeps constantly.
    fn drop_idle(entries: &mut Vec<CompiledEntry<T>>) {
        let now = std::time::Instant::now();
        entries.retain(|e| now.duration_since(e.last_used) < COMPILED_CACHE_IDLE);
    }
}

pub struct AgentRunner {
    engine: Engine,
    /// Built once. A linker describes what the host offers, which does not
    /// vary by turn, by guest, or by tenant.
    linker: Linker<AgentHost>,
    compiled: CompiledCache,
}

pub struct RunOptions {
    pub session_id: uuid::Uuid,
    pub gateway_url: String,
    pub gateway_token: String,
    pub default_model: String,
    pub progress: Option<ProgressSink>,
    pub on_tool: Option<ToolSink>,
    pub on_tool_result: Option<ToolResultSink>,
    pub fuel: u64,
    /// IANA zone of the user this turn belongs to, as the client reported it.
    /// Unrecognised or absent means the clock answers in UTC.
    pub timezone: Option<String>,
    /// Passed to providers that support it; ignored by those that do not.
    pub reasoning_effort: Option<String>,
    /// Names the class of traffic, which the gateway resolves to a route.
    pub traffic_type: String,
    /// Model calls permitted in this turn; zero is unbounded.
    pub max_tool_rounds: u32,
    /// The reply being written, so messages absorbed mid-turn can name it.
    pub reply_id: uuid::Uuid,
    /// Object storage the guest may reach, within its tenant's own space.
    pub storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
    /// Whose space that is. The guest is never told.
    pub tenant_id: uuid::Uuid,
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
        let engine = Engine::new(&config)?;

        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        // The generated linker needs a HasData marker naming which type the
        // host implementations live on.
        AgentWorld::add_to_linker::<_, HostData>(&mut linker, |state: &mut AgentHost| state)?;

        Ok(Self {
            engine,
            linker,
            compiled: CompiledCache::new(),
        })
    }

    /// The compiled form of these bytes, compiling only if it is not held.
    fn component_for(&self, bytes: &[u8]) -> anyhow::Result<Component> {
        use sha2::{Digest, Sha256};

        let key: [u8; 32] = Sha256::digest(bytes).into();
        if let Some(component) = self.compiled.get(&key) {
            return Ok(component);
        }

        let started = std::time::Instant::now();
        let component = Component::new(&self.engine, bytes)?;
        tracing::debug!(
            bytes = bytes.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "compiled a guest component"
        );

        self.compiled.insert(key, component.clone());
        Ok(component)
    }

    pub async fn run(
        &self,
        component_bytes: &[u8],
        conversation: Vec<Message>,
        system_prompt: String,
        options: RunOptions,
    ) -> anyhow::Result<(String, TurnCost)> {
        let component = self.component_for(component_bytes)?;

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
            on_tool_result: options.on_tool_result,
            session_id: options.session_id,
            // Parsed here so a bad zone from a client degrades to UTC once,
            // rather than on every call the guest makes.
            reasoning_effort: options.reasoning_effort,
            traffic_type: options.traffic_type,
            max_tool_rounds: options.max_tool_rounds,
            rounds_used: 0,
            arrivals: Vec::new(),
            reply_id: options.reply_id,
            spent: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            served_by: None,
            storage: options.storage,
            tenant_id: options.tenant_id,
            timezone: options.timezone.as_deref().and_then(|tz| {
                tz.parse::<chrono_tz::Tz>()
                    .inspect_err(|_| tracing::warn!(timezone = tz, "unknown timezone, using UTC"))
                    .ok()
            }),
        };

        let mut store = Store::new(&self.engine, host);
        store.set_fuel(options.fuel)?;

        let instance = AgentWorld::instantiate_async(&mut store, &component, &self.linker).await?;

        let reply = instance
            .outturn_agent_agent()
            .call_run(&mut store, &conversation, &system_prompt)
            .await?
            .map_err(|e| anyhow::anyhow!("guest returned an error: {e}"))?;

        // Read back from the host rather than returned by the guest: the
        // guest never sees these numbers, which is the point.
        let host = store.data();
        let cost = TurnCost {
            prompt_tokens: host.spent.prompt_tokens,
            completion_tokens: host.spent.completion_tokens,
            cache_read_tokens: host.spent.cache_read_tokens,
            cache_write_tokens: host.spent.cache_write_tokens,
            reasoning_tokens: host.spent.reasoning_tokens,
            provider: host.served_by.clone(),
        };

        Ok((reply, cost))
    }
}

#[cfg(test)]
mod compiled_cache_tests {
    use super::*;

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn cache() -> CompiledCache<u32> {
        CompiledCache::new()
    }

    #[test]
    fn what_was_put_in_comes_back_out() {
        let c = cache();
        assert_eq!(c.get(&key(1)), None);
        c.insert(key(1), 11);
        assert_eq!(c.get(&key(1)), Some(11));
    }

    #[test]
    fn different_bytes_never_share_a_build() {
        // The whole reason for keying on content: a changed component must
        // not be served the build of the one it replaced.
        let c = cache();
        c.insert(key(1), 11);
        c.insert(key(2), 22);
        assert_eq!(c.get(&key(1)), Some(11));
        assert_eq!(c.get(&key(2)), Some(22));
    }

    #[test]
    fn a_full_cache_drops_the_least_recently_used() {
        let c = cache();
        for n in 0..COMPILED_CACHE_ENTRIES as u8 {
            c.insert(key(n), n as u32);
        }
        // Touch the oldest so it is no longer the one to go.
        assert_eq!(c.get(&key(0)), Some(0));
        c.insert(key(200), 200);

        assert_eq!(c.get(&key(0)), Some(0), "a recently used entry was evicted");
        assert_eq!(c.get(&key(1)), None, "the least recently used entry survived");
        assert_eq!(c.get(&key(200)), Some(200));
    }

    #[test]
    fn a_build_nobody_asks_for_does_not_stay_resident() {
        // A node serving one agent never fills the cache, so the size cap
        // would keep a superseded build for the life of the process.
        let c = cache();
        c.insert(key(1), 11);
        {
            let mut entries = c.entries.lock().expect("lock");
            entries[0].last_used -= COMPILED_CACHE_IDLE + std::time::Duration::from_secs(1);
        }
        assert_eq!(c.get(&key(1)), None, "an idle build outlived its welcome");
        assert!(c.entries.lock().expect("lock").is_empty());
    }

    #[test]
    fn staying_in_use_keeps_a_build_alive() {
        let c = cache();
        c.insert(key(1), 11);
        for _ in 0..3 {
            assert_eq!(c.get(&key(1)), Some(11));
        }
        assert_eq!(c.entries.lock().expect("lock").len(), 1);
    }
}
