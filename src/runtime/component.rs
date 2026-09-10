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
    Arrival, Clock, Completion, CompletionRequest, ContentPart, HttpRequest, HttpResponse, Limits,
    Message, ObjectInfo, ToolActivity,
    ToolCall, ToolDefinition, ToolOutcome, Usage,
};

/// The OpenAI-shaped projection of an ordered message: the text joined, the
/// calls listed after it.
///
/// That protocol has no way to say a call came between two pieces of text, so
/// anything sent over it loses the arrangement. The parts travel beside it
/// under `outturn.parts` for providers that can say it -- see
/// `openai_to_anthropic` in the gateway.
pub(crate) fn flatten_parts(parts: &[ContentPart]) -> (String, Vec<ToolCall>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text(t) => text.push_str(t),
            ContentPart::Call(c) => calls.push(c.clone()),
        }
    }
    (text, calls)
}

/// Reports text as the model produces it, before the turn finishes.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Reports a tool call as the guest starts it.
pub type ToolSink = Arc<dyn Fn(&ToolActivity) + Send + Sync>;

/// Reports what a tool produced, for the reader rather than the model.
pub type ToolResultSink = Arc<dyn Fn(&ToolOutcome) + Send + Sync>;

/// What one model call cost, and who served it.
///
/// Reported per call rather than summed, because a bill is cut per call: a
/// turn that fell back to a second provider mid-way has two of these, and a
/// turn that failed after three calls still has three.
#[derive(Debug, Clone)]
pub struct CallUsage {
    pub round: u32,
    pub endpoint: String,
    pub model: String,
    pub paid_by: String,
    pub usage: Usage,
    /// The provider's usage object as it came off the wire, for the ledger.
    pub provider_usage: Option<serde_json::Value>,
    pub service_tier: Option<String>,
}

/// Reports each model call's cost as it completes.
pub type UsageSink = Arc<dyn Fn(&CallUsage) + Send + Sync>;

/// How long a request an agent made may take.
///
/// Far shorter than a model call, because this is a request to somebody else's
/// service on behalf of an agent that is holding a turn open while it waits.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How much of a response may come back.
///
/// The body is written by somebody else and a response that never ends is a
/// way to exhaust a pod that was told to be careful about memory. What arrives
/// past this is dropped, and the agent is told plainly that it was.
const FETCH_BODY_LIMIT: usize = 256 * 1024;

/// Removes URLs from an error before it is shown to a model.
///
/// A URL an agent built can carry a credential in its query string, and these
/// strings go into a transcript that is read back on every later turn.
fn strip_url(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            if word.contains("://") {
                "<url>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

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
    /// The platform's temperature for this turn, applied when the guest
    /// names none. Resolved above the runtime; the guest never sees where
    /// it came from.
    temperature: Option<f32>,
    on_tool: Option<ToolSink>,
    on_tool_result: Option<ToolResultSink>,
    on_usage: Option<UsageSink>,
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
    /// workspace, and asking it to declare its own spend is asking the party
    /// being billed to write the invoice.
    spent: Usage,
    /// Which endpoint served this turn, as the gateway reported it.
    served_by: Option<String>,
    /// Whether any round of this turn has streamed text yet. Decides whether
    /// the next round's first token is preceded by a paragraph break.
    streamed: bool,
    /// Object storage, and the space this turn may touch within it. Absent
    /// leaves the guest with no storage at all rather than with somebody
    /// else's.
    storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
    workspace_id: uuid::Uuid,
    space: crate::runtime::storage::scope::Space,
    /// Scopes the guest may write, resolved above the runtime. Reads are
    /// always allowed within the space; a guest that could not read its own
    /// workspace's reference material could not do its job.
    write_scopes: Vec<crate::runtime::storage::scope::Scope>,
    /// Hosts this workspace's agents may reach. Empty means none, which is what a
    /// workspace who has not thought about it has consented to.
    egress: Vec<crate::runtime::egress::EgressRule>,
    /// What the guest may grow to. Consulted by wasmtime on every memory or
    /// table growth; a request past it fails inside the guest rather than
    /// being granted and killing the pod.
    limits: wasmtime::StoreLimits,
}

/// What separates the text of one model round from the next in a reply.
///
/// The guest joins rounds with the same string when it returns the reply, so
/// a component that does otherwise breaks the invariant that streamed deltas
/// concatenate to stored content -- see the note on `chat` in the WIT.
pub const ROUND_SEPARATOR: &str = "\n\n";

/// Linear memory one guest may hold.
///
/// Above what any reasonable turn needs and below what would take a pod with
/// it: a 512Mi pod carrying several turns cannot afford one of them growing
/// to a gigabyte. A guest that hits this sees allocation fail and can report
/// it; the alternative is the kernel reporting it for everyone.
pub const GUEST_MEMORY_LIMIT: usize = 128 * 1024 * 1024;

/// Fuel burned between yields to the executor.
///
/// Small next to the turn's total, large next to the cost of a yield: on the
/// order of a millisecond of work.
const FUEL_YIELD_INTERVAL: u64 = 10_000_000;

impl AgentHost {
    /// Turns a storage failure into what the guest is told, and decides who
    /// else hears about it.
    ///
    /// A platform fault -- no bucket, no connection, bad credentials -- is
    /// logged at warn, because an operator has to fix it and nothing on the
    /// transcript reaches one. Everything else is the workspace's outcome: the
    /// transcript already records it as a failed tool, and putting it in the
    /// process log would page the operator for somebody else's typo.
    fn storage_failed(&self, what: &str, e: crate::runtime::storage::StorageError) -> String {
        if e.is_platform_fault() {
            tracing::warn!(
                session_id = %self.session_id,
                workspace_id = %self.workspace_id,
                error = %e,
                "object storage failed during {what}"
            );
        } else {
            tracing::debug!(session_id = %self.session_id, error = %e, "{what} refused");
        }
        e.to_string()
    }

    /// Resolves a guest path, or refuses it.
    ///
    /// The check is here rather than in the guest for the same reason the
    /// credential is: a component is deployed by a workspace, and a boundary it
    /// enforces on itself is not a boundary.
    fn object_at(
        &self,
        path: &str,
    ) -> Result<(Arc<dyn crate::runtime::storage::StorageBackend>, String), String> {
        use crate::runtime::storage::StorageError;
        let Some(storage) = self.storage.clone() else {
            return Err("no object storage is configured".to_string());
        };
        let resolved = crate::runtime::storage::scope::resolve(&self.space, path).map_err(|e| match e {
            // Said in full: this is the one a model will hit, and the message
            // tells it how to correct itself.
            StorageError::Refused(m) => m,
            _ => format!("path is not allowed: {path}"),
        })?;
        Ok((storage, resolved))
    }

    /// Whether this turn may write at `path`, by the scope it names.
    fn may_write(&self, path: &str) -> Result<(), String> {
        let (scope, _) = crate::runtime::storage::scope::split(path).map_err(|e| e.to_string())?;
        if self.write_scopes.contains(&scope) {
            Ok(())
        } else {
            Err(format!(
                "this agent may read {0}/ but not write to it. Write under session/ instead, \
                 or ask whoever runs the workspace to allow writes to {0}/.",
                scope.as_str()
            ))
        }
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
                let (content, tool_calls) = flatten_parts(&m.parts);
                let mut value = serde_json::json!({
                    "role": m.role,
                    "content": content,
                });
                // Sent beside the flattening, not instead of it: a provider
                // that can hold the order gets it, and one that cannot reads
                // the fields it already understands and ignores this.
                if m.parts.len() > 1 {
                    value["outturn"] = serde_json::json!({
                        "parts": m.parts.iter().map(|p| match p {
                            ContentPart::Text(t) => serde_json::json!({"type": "text", "text": t}),
                            ContentPart::Call(c) => serde_json::json!({
                                "type": "tool_call",
                                "id": c.id,
                                "name": c.name,
                                "arguments": c.arguments,
                            }),
                        }).collect::<Vec<_>>()
                    });
                }
                if !tool_calls.is_empty() {
                    value["tool_calls"] = serde_json::json!(
                        tool_calls
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
            // The guest's choice if it made one, else the platform's.
            "temperature": request.temperature.or(self.temperature),
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

        // Rounds of one turn are separated by a paragraph break, and the
        // break has to reach the browser *before* the round's first token.
        // The guest cannot put it there: it learns a round produced text
        // only when `chat` returns, by which time the text has streamed. So
        // the host inserts it here, at the first token, when an earlier
        // round has already shown something -- and the guest joins rounds
        // with the same "\n\n" when it assembles the reply, so what streamed
        // and what is stored stay the same string.
        let progress = self.progress.as_ref().map(|sink| {
            let sink = Arc::clone(sink);
            let separate = self.streamed;
            let pending = std::sync::atomic::AtomicBool::new(separate);
            Arc::new(move |text: &str| {
                if pending.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    sink(ROUND_SEPARATOR);
                }
                sink(text);
            }) as ProgressSink
        });

        let (completion, arrivals, served) = stream_completion(
            &self.http,
            &self.gateway_url,
            &self.gateway_token,
            &self.traffic_type,
            &self.reply_id,
            body,
            progress.as_ref(),
        )
        .await
        .map_err(|e| e.to_string())?;

        if completion
            .parts
            .iter()
            .any(|p| matches!(p, ContentPart::Text(t) if !t.is_empty()))
        {
            self.streamed = true;
        }

        // Billed per call, the moment it is known. `rounds_used` was counted
        // before the call, so the round is one less.
        if let (Some(sink), Some(usage)) = (&self.on_usage, &completion.usage) {
            sink(&CallUsage {
                round: self.rounds_used.saturating_sub(1),
                endpoint: served.endpoint.clone().unwrap_or_default(),
                model: served.model.clone().unwrap_or(model.clone()),
                paid_by: served.paid_by.clone().unwrap_or_else(|| "operator".to_string()),
                usage: usage.clone(),
                provider_usage: served.provider_usage.clone(),
                service_tier: served.service_tier.clone(),
            });
        }
        let served_by = served.endpoint;

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

    /// Makes a request an agent asked for, if everything about it is allowed.
    ///
    /// Every refusal comes back as an error the model can read, because a tool
    /// that fails opaquely gets called again the same way. None of them tell
    /// the model anything it could use: that a host is not allowed is a fact
    /// about the workspace's settings, and that one resolves inside the cluster is
    /// a fact it already had to guess to ask.
    async fn fetch(&mut self, request: HttpRequest) -> Result<HttpResponse, String> {
        use crate::runtime::egress;

        let method = match request.method.to_ascii_uppercase().as_str() {
            m @ ("GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD") => m.to_string(),
            other => return Err(format!("{other} is not a method this can send")),
        };

        let url = reqwest::Url::parse(&request.url).map_err(|e| format!("that URL is not one: {e}"))?;
        let (host, rule) = egress::check_url(&self.egress, &url).map_err(|e| e.to_string())?;

        // A credential travels only where it cannot be read on the way. The
        // rule names the host; the request names the scheme; and a workspace who
        // configured a key for a host did not consent to it going out in
        // clear because a model typed http.
        if rule.credential_env.is_some() && url.scheme() != "https" {
            return Err(format!(
                "{host} has a credential configured, so it can only be reached over https"
            ));
        }

        // Resolved once, and the connection pinned to the answer. Checking a
        // name and then letting the client look it up again is a check of a
        // different request to the one that gets made.
        let port = url
            .port_or_known_default()
            .ok_or_else(|| "that URL names no port and its scheme implies none".to_string())?;
        let addrs = egress::resolve_and_vet(&host, port)
            .await
            .map_err(|e| e.to_string())?;

        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &request.headers {
            egress::check_header(name).map_err(|e| e.to_string())?;
            let name: reqwest::header::HeaderName = name
                .parse()
                .map_err(|_| format!("{name} is not a header name"))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| format!("the {name} header's value cannot be sent"))?;
            headers.insert(name, value);
        }

        // Attached after the guest's headers, so nothing it sent can displace
        // one, and read from the environment rather than from anything that
        // crossed the sandbox boundary.
        if let (Some(header), Some(variable)) = (&rule.header, &rule.credential_env) {
            let secret = std::env::var(variable).map_err(|_| {
                // Names the variable, not its absence from any particular
                // place: whoever reads this configured the rule.
                format!("this host's credential ({variable}) is not configured")
            })?;
            let name: reqwest::header::HeaderName = header
                .parse()
                .map_err(|_| format!("{header} is not a header name"))?;
            let mut value = reqwest::header::HeaderValue::from_str(&secret)
                .map_err(|_| "this host's credential cannot be sent as a header".to_string())?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }

        let mut client = reqwest::Client::builder()
            .connect_timeout(crate::http_client::CONNECT_TIMEOUT)
            .timeout(FETCH_TIMEOUT)
            // A redirect names a host that was never checked. Refusing to
            // follow is what keeps an allowed host from being a doorway.
            .redirect(reqwest::redirect::Policy::none());
        client = client.resolve_to_addrs(&host, &addrs);
        let client = client
            .build()
            .map_err(|e| format!("could not prepare the request: {e}"))?;

        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| "that is not a method this can send".to_string())?;
        let mut outgoing = client.request(method, url).headers(headers);
        if let Some(body) = request.body {
            outgoing = outgoing.body(body);
        }

        let response = outgoing.send().await.map_err(|e| {
            // The URL is stripped: it can carry a credential in a query
            // string, and this text goes to a model and into a transcript.
            format!("the request did not complete: {}", strip_url(&e.to_string()))
        })?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();

        // Read to a bound rather than to the end. A response is written by
        // somebody else, and a body that does not stop is a way to exhaust a
        // pod that was told to be careful about memory. Chunks are taken
        // until the limit is passed and the connection is then dropped, so
        // what is held in memory is never more than one chunk over the bound.
        let mut body: Vec<u8> = Vec::new();
        let mut truncated = false;
        {
            use futures::StreamExt;
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|e| {
                    format!("the response did not arrive whole: {}", strip_url(&e.to_string()))
                })?;
                body.extend_from_slice(&chunk);
                if body.len() > FETCH_BODY_LIMIT {
                    truncated = true;
                    body.truncate(FETCH_BODY_LIMIT);
                    break;
                }
            }
        }
        let body = String::from_utf8_lossy(&body).to_string();

        Ok(HttpResponse {
            status,
            headers,
            body,
            truncated,
        })
    }

    async fn read_object(
        &mut self,
        path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        let (storage, resolved) = self.object_at(&path)?;

        // A document is handed over as the words in it, not as the bytes a
        // model can do nothing with. The guest is never told this happened --
        // it asked for `session/report.pdf` and gets prose -- for the same
        // reason settings are resolved above it: what it needs is the content,
        // and where the content came from is the host's business.
        if crate::api::extract::is_extractable(&resolved) {
            let text = crate::api::extract::text_key(&resolved);
            match storage.read(&text, offset, len).await {
                // Extracted, and there was nothing in it. Said outright: an
                // empty read is indistinguishable from an empty document, and
                // a model told a report is blank will report that it is.
                Ok(bytes) if bytes.is_empty() && offset == 0 => {
                    return Err(format!(
                        "no text could be read from {path}; it may be a scan or an image"
                    ));
                }
                Ok(bytes) => return Ok(bytes),
                Err(crate::runtime::storage::StorageError::NotFound) => {
                    // Said plainly rather than answered with the raw bytes or
                    // with nothing. An empty read is indistinguishable from an
                    // empty document, and a model told a report is blank will
                    // confidently report that it is.
                    return Err(format!(
                        "{path} is still being read; ask again shortly"
                    ));
                }
                Err(e) => return Err(self.storage_failed("read", e)),
            }
        }

        storage
            .read(&resolved, offset, len)
            .await
            .map_err(|e| self.storage_failed("read", e))
    }

    async fn stat_object(&mut self, path: String) -> Result<ObjectInfo, String> {
        let (storage, resolved) = self.object_at(&path)?;
        let found = storage
            .stat(&resolved)
            .await
            .map_err(|e| self.storage_failed("stat", e))?;
        Ok(ObjectInfo {
            // Handed back as the guest named it, not as it is stored.
            path,
            size: found.size,
        })
    }

    async fn write_object(&mut self, path: String, data: Vec<u8>) -> Result<u64, String> {
        self.may_write(&path)?;
        let (storage, resolved) = self.object_at(&path)?;
        storage
            .write(&resolved, 0, &data)
            .await
            .map_err(|e| self.storage_failed("write", e))
    }

    async fn list_objects(&mut self, prefix: String) -> Result<Vec<ObjectInfo>, String> {
        use crate::runtime::storage::scope::{self, Scope};
        let Some(storage) = self.storage.clone() else {
            return Err("no object storage is configured".to_string());
        };
        // An empty prefix means "everything I have": the three scopes, each
        // listed under its own name.
        let prefixes: Vec<String> = if prefix.trim().is_empty() {
            Scope::ALL.iter().map(|s| scope::root_for(&self.space, *s)).collect()
        } else {
            vec![scope::resolve_prefix(&self.space, &prefix).map_err(|e| match e {
                crate::runtime::storage::StorageError::Refused(m) => m,
                other => other.to_string(),
            })?]
        };

        let mut out = Vec::new();
        for resolved in prefixes {
            let found = storage
                .list(&resolved)
                .await
                .map_err(|e| self.storage_failed("list", e))?;
            out.extend(found.iter().filter(|f| !f.is_dir).filter_map(|f| {
                Some(ObjectInfo {
                    path: scope::strip_root(&self.space, &f.path)?,
                    size: f.size,
                })
            }));
        }
        Ok(out)
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
) -> anyhow::Result<(Completion, Vec<Arrival>, Served)> {
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
    // for it rather than to whichever one was configured first, and to the
    // credential that paid.
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let mut served = Served {
        endpoint: header("x-outturn-provider"),
        paid_by: header("x-outturn-paid-by"),
        model: None,
        provider_usage: None,
        service_tier: None,
    };

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    // Built in arrival order. A tool call is assembled across several chunks,
    // so its place in the sequence is where it first appeared, not where it
    // finished -- which is what `call_slots` remembers.
    let mut parts: Vec<ContentPart> = Vec::new();
    let mut call_slots: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    let mut finish_reason = None;
    let mut usage = None;
    // Tool calls arrive in fragments keyed by index, and the arguments are a
    // JSON string spread across chunks. Kept sparse by index rather than
    // pushed, since a provider is free to interleave two calls.
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

            // The model that actually answered, which a route may have
            // chosen in place of the one asked for.
            if served.model.is_none()
                && let Some(m) = chunk["model"].as_str()
                && !m.is_empty()
            {
                served.model = Some(m.to_string());
            }
            if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str()
                && !text.is_empty()
            {
                match parts.last_mut() {
                    Some(ContentPart::Text(t)) => t.push_str(text),
                    _ => parts.push(ContentPart::Text(text.to_string())),
                }
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
                    let slot = *call_slots.entry(index).or_insert_with(|| {
                        parts.push(ContentPart::Call(ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        }));
                        parts.len() - 1
                    });
                    let Some(ContentPart::Call(entry)) = parts.get_mut(slot) else {
                        continue;
                    };
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
            if let Some(tier) = chunk["service_tier"].as_str() {
                served.service_tier = Some(tier.to_string());
            }
            if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
                // Kept whole. The fields read below are the ones every bill
                // needs; the rest are the ones a later bill might.
                served.provider_usage = Some(u.clone());
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
        // A call with no name is a fragment of something that never arrived;
        // passing it on would have the guest dispatch on "".
        parts: parts
            .into_iter()
            .filter(|p| !matches!(p, ContentPart::Call(c) if c.name.is_empty()))
            .collect(),
        finish_reason,
        usage,
    }, arrivals, served))
}

/// Who answered a call, as far as the gateway said.
struct Served {
    endpoint: Option<String>,
    model: Option<String>,
    paid_by: Option<String>,
    provider_usage: Option<serde_json::Value>,
    service_tier: Option<String>,
}


/// Compiled components to keep, keyed by the bytes they came from.
///
/// A cap because the map is keyed by workspace-supplied content: without one,
/// uploading a component is a way to make the runtime allocate, over and over,
/// with nothing to reclaim it. Small, because in practice a node serves a
/// handful of distinct agents and a miss costs one compile, not a failure.
const COMPILED_CACHE_ENTRIES: usize = 32;

/// How long an unused compiled component is kept.
///
/// Long enough that an idle workspace does not pay a compile on every message,
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
    /// vary by turn, by guest, or by workspace.
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
    pub on_usage: Option<UsageSink>,
    pub fuel: u64,
    /// IANA zone of the user this turn belongs to, as the client reported it.
    /// Unrecognised or absent means the clock answers in UTC.
    pub timezone: Option<String>,
    /// Passed to providers that support it; ignored by those that do not.
    pub reasoning_effort: Option<String>,
    /// Applied when the guest names no temperature of its own.
    pub temperature: Option<f32>,
    /// Names the class of traffic, which the gateway resolves to a route.
    pub traffic_type: String,
    /// Model calls permitted in this turn; zero is unbounded.
    pub max_tool_rounds: u32,
    /// The reply being written, so messages absorbed mid-turn can name it.
    pub reply_id: uuid::Uuid,
    /// Object storage the guest may reach, within its workspace's own space.
    pub storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
    /// Hosts this turn may reach, from the workspace's own rules.
    pub egress: Vec<crate::runtime::egress::EgressRule>,
    /// Whose space that is. The guest is never told.
    pub workspace_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    /// Scopes the guest may write: "session", "agent", "workspace".
    pub write_scopes: Vec<String>,
    /// How long the gateway's stream may go silent before the turn is
    /// abandoned. A parameter so a test can prove it fires.
    pub idle_timeout: std::time::Duration,
}

impl AgentRunner {
    pub fn new() -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
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
            on_usage: options.on_usage,
            session_id: options.session_id,
            // Parsed here so a bad zone from a client degrades to UTC once,
            // rather than on every call the guest makes.
            reasoning_effort: options.reasoning_effort,
            temperature: options.temperature,
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
            streamed: false,
            storage: options.storage,
            workspace_id: options.workspace_id,
            space: crate::runtime::storage::scope::Space {
                workspace_id: options.workspace_id,
                agent_id: options.agent_id,
                session_id: options.session_id,
            },
            write_scopes: options
                .write_scopes
                .iter()
                .filter_map(|s| crate::runtime::storage::scope::Scope::parse(s))
                .collect(),
            egress: options.egress,
            limits: wasmtime::StoreLimitsBuilder::new()
                .memory_size(GUEST_MEMORY_LIMIT)
                // One instance and a handful of tables is what a component
                // needs; more is a guest doing something it need not.
                .instances(8)
                .tables(64)
                .build(),
            timezone: options.timezone.as_deref().and_then(|tz| {
                tz.parse::<chrono_tz::Tz>()
                    .inspect_err(|_| tracing::warn!(timezone = tz, "unknown timezone, using UTC"))
                    .ok()
            }),
        };

        let mut store = Store::new(&self.engine, host);
        store.set_fuel(options.fuel)?;
        // Yield to the executor every so often while fuel is burning. Fuel
        // alone bounds how long a guest may run, not how long it may hold the
        // thread it runs on: without this a busy loop pins a Tokio worker
        // until the whole budget is spent, and every other turn on that
        // thread waits for it.
        store.fuel_async_yield_interval(Some(FUEL_YIELD_INTERVAL))?;
        // The guest's memory is capped here, in the sandbox, not only guessed
        // at by admission. Admission decides whether to start a turn from the
        // memory that is free; nothing about that stops a component from
        // growing once it is running, and a guest that grows without bound
        // takes the pod -- and every other turn on it -- with it.
        store.limiter(|host| &mut host.limits);

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

#[cfg(test)]
mod artifact_guard {
    /// The committed component must have been built against the current
    /// interface.
    ///
    /// `assets/agent_default.wasm` and `agents/default/src/bindings.rs` are both
    /// generated and both committed, and neither regenerates on its own. Change
    /// `wit/agent.wit` without rebuilding and the guest crate still compiles --
    /// against its stale bindings -- while every turn fails at runtime with
    /// "component imports instance `outturn:agent/host`, but a matching
    /// implementation was not found in the linker". That is a long way to travel
    /// for a mismatch a file comparison can catch.
    ///
    /// The interface is copied beside the component when it is built, so this
    /// compares the two and a failure can be read as a diff.
    #[test]
    fn the_committed_component_was_built_against_this_interface() {
        const BUILT_AGAINST: &str = include_str!("../../assets/agent_default.wit");
        const CURRENT: &str = include_str!("../../wit/agent.wit");

        assert_eq!(
            CURRENT, BUILT_AGAINST,
            "wit/agent.wit has changed since assets/agent_default.wasm was built.\n\
             Rebuild the guest and copy both artifacts:\n\
             \x20 (cd agents/default && cargo component build --release)\n\
             \x20 cp agents/default/target/wasm32-wasip1/release/outturn_agent_default.wasm \
             assets/agent_default.wasm\n\
             \x20 cp wit/agent.wit assets/agent_default.wit\n\
             `cargo component build` regenerates src/bindings.rs on the way, so it \
             covers both."
        );
    }
}
