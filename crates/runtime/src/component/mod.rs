//! Host side of the agent component interface.
//!
//! Replaces the pointer-and-length imports of the preview 1 sandbox: values
//! cross the boundary as typed records, so there is no manual marshalling and
//! no bounds-checking of guest-supplied offsets.

//!
//! Split by capability: storage, model calls and the stream they read,
//! outbound requests, timers and the clock each have a module, and the
//! `Host` impl here only hands each call to the one that serves it.

mod cache;
mod clock;
mod egress;
mod model;
mod runner;
mod storage;
mod stream;
mod timers;

use std::sync::Arc;

use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxView, WasiView};

pub use runner::{AgentRunner, Finished, RunOptions};
pub use storage::{MAX_OPEN_WRITERS, MAX_READ_CHUNK, ReaderState, WriterState, clamp_read};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "agent-world",
    imports: { default: async },
    exports: { default: async },
    with: {
        "outturn:agent/host.object-writer": WriterState,
        "outturn:agent/host.object-reader": ReaderState,
    },
});

pub use outturn::agent::host::{
    Arrival, Clock, Completion, CompletionRequest, ContentPart, HttpRequest, HttpResponse, Limits,
    Message, ObjectInfo, SkillOperation, TimerDue, ToolActivity, ToolCall, ToolDefinition,
    ToolOutcome, Usage,
};

/// Reports text as the model produces it, before the turn finishes.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Reports a model's thinking as it produces it.
///
/// Separate from `ProgressSink` because thinking is not the reply: it never
/// joins the stored content, never reaches a later turn, and never gets the
/// round separator a second round's prose does. The guest does not see it at
/// all -- which model thinks aloud is a provider detail, and an agent that read
/// its own reasoning back would answer it.
pub type ReasoningSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Reports that the model is writing a call to a tool, with the tool's name,
/// its place among the round's calls, and how many bytes of arguments it has
/// written so far.
///
/// Said first with none, as soon as the call has a name, and then about once
/// a second while the arguments grow. The arguments themselves are not sent:
/// they are not whole, and so not runnable or worth showing, until the round
/// ends. Without the first, the reader saw nothing between the model's last
/// thought and the call starting. Without the rest, a call carrying a whole
/// document -- minutes of writing, for a model that loops -- was a spinner
/// that said nothing about whether anything was happening.
pub type WritingSink = Arc<dyn Fn(u32, &str, u64) + Send + Sync>;

/// Reports a tool call as the guest starts it.
pub type ToolSink = Arc<dyn Fn(&ToolActivity) + Send + Sync>;

/// Reports what a tool produced, for the reader rather than the model.
pub type ToolResultSink = Arc<dyn Fn(&ToolOutcome) + Send + Sync>;

/// Reports the messages the guest has just been handed mid-turn, by id, at
/// the moment it took them -- which is where in the reply they belong.
pub type AbsorbedSink = Arc<dyn Fn(&[uuid::Uuid]) + Send + Sync>;
/// Told when a request went out with a person's own key: the host, and whose.
pub type ConnectionSink = Arc<dyn Fn(&str, uuid::Uuid) + Send + Sync>;

/// What one model call cost, and who served it.
///
/// Reported per call rather than summed, because a bill is cut per call: a
/// turn that fell back to a second provider mid-way has two of these, and a
/// turn that failed after three calls still has three.
#[derive(Debug, Clone)]
pub struct CallUsage {
    pub round: u32,
    /// The class of traffic the call went out as. A turn's rounds are its
    /// own traffic type; a look at an image is `vision`, so the ledger can
    /// tell what a picture cost from what the conversation did.
    pub traffic_type: String,
    pub endpoint: String,
    pub model: String,
    pub paid_by: String,
    pub usage: Usage,
    /// The provider's usage object as it came off the wire, for the ledger.
    pub provider_usage: Option<serde_json::Value>,
    pub service_tier: Option<String>,
    /// The call ended before the model finished, so the numbers are for a
    /// partial answer.
    pub partial: bool,
    /// The numbers are this host's estimate, because the stream was cut
    /// before the provider reported anything.
    pub estimated: bool,
}

/// Reports each model call's cost as it completes.
pub type UsageSink = Arc<dyn Fn(&CallUsage) + Send + Sync>;

/// What became of an object the guest touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    /// Written, at this many bytes.
    Wrote(u64),
    Deleted,
}

/// Reports an object the guest wrote or deleted, as `(scoped path, resolved
/// key, what became of it)`: the API counts it against the workspace's
/// storage, and extracts a document written.
pub type WriteSink = Arc<dyn Fn(&str, &str, Stored) + Send + Sync>;

/// Told when the gateway refused a request because somebody has to approve it.
///
/// Async because it is an HTTP call to the API, and fallible because the answer
/// decides what the guest is told: `true` means an approval is now pending and
/// the turn will park, `false` means it was let through or could not be raised.
///
/// Carries the shape of the refused request and nothing else. Naming the act is
/// the API's to do -- see `api::gated` -- because a `requires` chosen in this
/// tier would be a policy decision made by the sandbox's own host.
/// Trades the current turn token, and the gates it commits to, for a fresh
/// token -- and a new gate set, when the API took a policy out of it.
///
/// `None` when the API would not reissue it; the caller keeps the token it has
/// and lets the gateway say whether it is still good.
pub type TokenRefresher = Arc<
    dyn Fn(
            String,
            outturn_core::egress::gate::RawGates,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Option<outturn_core::protocol::work::TokenRefreshed>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;

/// The record of declared writes, as this tier asks it: claim a key before
/// sending, and settle it with the answer -- or with none, releasing it when
/// the recipient refused. See `docs/idempotency.md`.
pub trait InvocationRecord: Send + Sync {
    fn claim(
        &self,
        key: String,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<outturn_core::protocol::work::Claimed, String>>
                + Send,
        >,
    >;
    fn settle(
        &self,
        key: String,
        response: Option<serde_json::Value>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
}

pub type GatedSink = Arc<
    dyn Fn(GatedRequest) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// A request to be woken later, as the guest made it.
///
/// Relayed to the API rather than acted on here: a wakeup is a job, a sleep is a
/// hold on the conversation and an item in somebody's queue, and all of it lives
/// in the tier with the database. The runtime executes workspace code and holds
/// none of those.
#[derive(Debug, Clone)]
pub enum WaitRequest {
    Sleep {
        seconds: u64,
        reason: String,
    },
    Timer {
        /// Exactly one of these, as the guest checked and the API checks again.
        at: Option<String>,
        seconds: Option<u64>,
        reason: String,
    },
    ListTimers,
    CancelTimer {
        id: String,
    },
}

/// Relays a wait to the API and returns what to tell the model, or why not.
pub type WaitSink = Arc<
    dyn Fn(
            WaitRequest,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

/// Tells the API what became of a held request, and hears back what the model
/// should read in its refusal's place. `None` when the API could not be told,
/// and the host then says what it knows itself.
pub type HeldSink = Arc<
    dyn Fn(
            uuid::Uuid,
            outturn_core::egress::held::Outcome,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
        + Send
        + Sync,
>;

/// Tells the API a held request is about to go, and hears whether it may.
/// False when the API refused or could not be told: then it is not sent.
pub type HeldBegin = Arc<
    dyn Fn(uuid::Uuid) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// Requests a person approved, to send before the guest runs.
pub struct HeldSends {
    pub calls: Vec<outturn_core::egress::held::HeldCall>,
    /// Minted for these alone: its commitment holds their grants, which the
    /// turn's own token does not.
    pub token: String,
    pub gates: outturn_core::egress::gate::RawGates,
    /// Asked before each is sent. Absent, each is sent unasked.
    pub begin: Option<HeldBegin>,
    pub report: Option<HeldSink>,
}

/// What was refused, as the runtime saw it: the whole request, as it was
/// about to go.
#[derive(Debug, Clone)]
pub struct GatedRequest {
    pub method: String,
    pub host: String,
    pub path: String,
    /// With its query, as the guest wrote it.
    pub url: String,
    /// The guest's headers, with any idempotency key this host added.
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub operation: Option<String>,
}

/// Marker tying the generated host traits to AgentHost.
struct HostData;

impl AgentHost {
    /// The turn token to present, replaced first if it is close to running out.
    ///
    /// Checked before every gateway call rather than at round boundaries: a
    /// round can hold one long model call and then a tool call, and the token
    /// that was good when the round started can lapse between them -- which is
    /// how an eight-minute turn failed its `fetch_url` with "token expired".
    ///
    /// A refusal to reissue is not fatal here. The old token may still have
    /// minutes in it, and if it has not the gateway will say so on the call,
    /// which is a better-placed error than one raised speculatively.
    async fn current_token(&mut self) -> String {
        let due = self.gateway_token_expires_at.is_some_and(|at| {
            at - chrono::Utc::now()
                < chrono::Duration::seconds(
                    outturn_core::auth::TURN_TOKEN_REFRESH_BELOW_SECS as i64,
                )
        });
        if due && let Some(refresh) = &self.refresh_token {
            match refresh(self.gateway_token.clone(), self.gates.clone()).await {
                Some(refreshed) => {
                    self.gateway_token = refreshed.gateway_token;
                    self.gateway_token_expires_at = Some(refreshed.gateway_token_expires_at);
                    // Replaced together: the new token commits to the new set,
                    // and either alone is refused by the gateway.
                    if let Some(gates) = refreshed.gates {
                        self.gates = gates;
                    }
                }
                None => tracing::warn!("could not refresh this turn's gateway token"),
            }
        }
        self.gateway_token.clone()
    }
}

impl wasmtime::component::HasData for HostData {
    type Data<'a> = &'a mut AgentHost;
}

pub struct AgentHost {
    wasi: WasiCtx,
    table: ResourceTable,
    gateway_url: String,
    gateway_token: String,
    /// When `gateway_token` stops being good. `None` from an API that does not
    /// say, in which case the token is never refreshed -- what it always was.
    gateway_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    refresh_token: Option<TokenRefresher>,
    /// What the API said gates this turn, as the bytes it sent. Relayed to the
    /// gateway with every outbound request exactly as received, never written
    /// out again from this build's types: see `RawGates`. It is checked against
    /// the commitment in the turn token, which this tier cannot write.
    gates: outturn_core::egress::gate::RawGates,
    /// Told when the gateway refuses for want of an approval. Relays the shape
    /// of the request; decides nothing about it.
    on_gated: Option<GatedSink>,
    /// Where declared writes are recorded. Absent, nothing is: a write is
    /// sent as asked, as every write was before.
    invocations: Option<Arc<dyn InvocationRecord>>,
    on_wait: Option<WaitSink>,
    default_model: String,
    http: reqwest::Client,
    progress: Option<ProgressSink>,
    reasoning: Option<ReasoningSink>,
    writing: Option<WritingSink>,
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
    /// What each reply that called a tool was signed with, by its first
    /// call's id, kept for the rounds after it. Held here rather than handed
    /// to the guest: the guest replays the reply as text and calls, and this
    /// puts back what it never saw (docs/thinking.md).
    signed: std::collections::HashMap<String, Signed>,
    /// The platform's temperature for this turn, applied when the guest
    /// names none. Resolved above the runtime; the guest never sees where
    /// it came from.
    temperature: Option<f32>,
    on_tool: Option<ToolSink>,
    on_tool_result: Option<ToolResultSink>,
    on_usage: Option<UsageSink>,
    on_write: Option<WriteSink>,
    on_absorbed: Option<AbsorbedSink>,
    on_connection: Option<ConnectionSink>,
    /// The hosts a person's own key has been reported sent to this turn, so
    /// each is said once however many calls went there.
    connections_reported: std::collections::HashSet<(String, uuid::Uuid)>,
    /// Zero means unbounded.
    max_tool_rounds: u32,
    /// Tools the guest should offer without being asked, by name.
    ///
    /// The deployment's policy, not the component's: a guest with many tools
    /// can name them and hand over definitions on request, and this says which
    /// are worth the rounds they would otherwise cost. Empty defers everything.
    eager_tools: Vec<String>,
    /// The bound skills' operations, offered to the model as tools. Handed to
    /// the guest as the API sent them; see `docs/skill-operations.md`.
    skill_operations: Vec<outturn_core::protocol::work::SkillOperation>,
    /// Model calls made so far this turn, counted host-side so a guest that
    /// ignores `limits` still cannot exceed them.
    rounds_used: u32,
    /// Requests through the gateway and listings of storage made so far this
    /// turn, each against its own fixed limit; see `MAX_FETCHES_PER_TURN`.
    fetches_used: u32,
    lists_used: u32,
    /// Bytes written to files so far this turn, against `max_written_bytes`.
    bytes_written: u64,
    /// The workspace's limit on that; zero for none.
    max_written_bytes: u64,
    /// What the workspace had left to store when the turn began; `None` for
    /// no quota. Counted against with every byte written, overwrites
    /// included, so a turn can only stop short of the quota, never pass it.
    remaining_stored_bytes: Option<u64>,
    /// What the user has said since this turn began, as reported by the
    /// gateway on the responses it was already sending. Drained when the guest
    /// asks, so each message is injected once. Each keeps the id of the
    /// message it came from, which the guest never sees.
    arrivals: Vec<(Arrival, Option<uuid::Uuid>)>,
    /// Whether somebody has asked this turn to stop, as reported by the
    /// gateway on the same responses.
    ///
    /// It arrives this way for the same reason steering does: the runtime
    /// holds no database and no credentials, so anything it needs to learn
    /// mid-turn rides the one connection it already has open. Sticky once set
    /// -- a cancel is not withdrawn by the next round failing to mention it.
    cancelled: bool,
    /// Why a hold cut this turn, where one did.
    held: Option<String>,
    /// Whether what stopped it is a question somebody will answer.
    ///
    /// The difference decides whether the session latches. A spend cap or an
    /// operator's stop is meant to need a person to lift it; an approval is
    /// lifted *by* being answered, and latching would leave the conversation
    /// stopped after the yes.
    awaiting_approval: bool,
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
    /// And which model it was, as the gateway reported it: what a route
    /// actually sent, not what the agent asked for.
    served_model: Option<String>,
    /// Whether any round of this turn has streamed text yet. Decides whether
    /// the next round's first token is preceded by a paragraph break.
    streamed: bool,
    /// Object storage, and the space this turn may touch within it. Absent
    /// leaves the guest with no storage at all rather than with somebody
    /// else's.
    storage: Option<Arc<dyn outturn_core::storage::StorageBackend>>,
    workspace_id: uuid::Uuid,
    space: outturn_core::storage::scope::Space,
    /// Scopes the guest may write, resolved above the runtime. Always a subset
    /// of `read_scopes`: the setting that grants a write grants the read with
    /// it.
    write_scopes: Vec<outturn_core::storage::scope::Scope>,
    /// Scopes the guest may read, resolved above the runtime. Always a
    /// superset of `write_scopes`: the setting that grants a write grants the
    /// read with it, because a write extracts the file's text straight back
    /// out and "write but not read" would be a promise the host cannot keep.
    read_scopes: Vec<outturn_core::storage::scope::Scope>,
    /// The bound skills' files, by the name the guest reads them under.
    skill_files: Vec<outturn_core::storage::scope::SkillObject>,
    /// Hosts this workspace's agents may reach. Empty means none, which is what a
    /// workspace who has not thought about it has consented to.
    egress: Vec<outturn_core::egress::rule::EgressRule>,
    /// What the guest may grow to. Consulted by wasmtime on every memory or
    /// table growth; a request past it fails inside the guest rather than
    /// being granted and killing the pod.
    limits: wasmtime::StoreLimits,
    /// Asked before this turn holds memory beyond its flat charge -- an open
    /// writer. Absent charges nothing, which is what a test wants.
    admission: Option<Arc<crate::admission::Admission>>,
    /// Where the PDF renderer listens. None where none is deployed.
    renderer_url: Option<String>,
    open_writers: usize,
    /// Whether a round's reply came back unusable. See `HeldError`.
    malformed_reply: bool,
    /// Why the provider declined a round, if it did.
    refused: Option<outturn_core::protocol::work::Refusal>,
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
    /// Charges admission for memory this turn is about to hold, or says why
    /// it cannot. Nothing to charge when there is no admission to ask.
    fn charge(&self, bytes: u64, what: &str) -> Result<Option<crate::admission::Charge>, String> {
        let Some(admission) = &self.admission else {
            return Ok(None);
        };
        admission.try_charge(bytes).map(Some).map_err(|refusal| {
            tracing::debug!(session_id = %self.session_id, %refusal, "{what} refused");
            format!(
                "the runtime is short of memory and cannot {what} right now; \
                 wait a little and try again"
            )
        })
    }
}

/// A turn that failed, and whether a hold was what ended it.
///
/// A failure and a stop look identical from outside -- both end a turn without
/// a reply -- and they must not be treated alike. A failure is retried; a stop
/// that is retried runs the work the stop existed to prevent, as soon as
/// whoever took the hold releases it.
#[derive(Debug)]
pub struct HeldError {
    /// Why a hold cut this turn, where one did.
    pub held: Option<String>,
    pub error: anyhow::Error,
    /// Whether a model reply that could not be used is what ended it. Read
    /// from the host rather than from the error: the failure reaches the
    /// guest as a string, and the guest's error is what the turn returns.
    pub malformed_reply: bool,
    /// Why the provider declined, where that is what ended it. Read from the
    /// host for the same reason.
    pub refused: Option<outturn_core::protocol::work::Refusal>,
}

impl HeldError {
    fn new(held: Option<String>, error: anyhow::Error) -> Self {
        Self {
            held,
            error,
            malformed_reply: false,
            refused: None,
        }
    }
}

impl<E: Into<anyhow::Error>> From<E> for HeldError {
    /// For the failures that happen before a guest exists to be held --
    /// instantiating, fuel, the component itself. Nothing had cut them.
    fn from(error: E) -> Self {
        Self::new(None, error.into())
    }
}

/// What a turn that ran out of fuel says, in place of wasmtime's trap text.
///
/// Read by whoever has the conversation open, who did not choose the limit
/// and cannot act on a stack of wasm frames.
pub const OUT_OF_FUEL: &str = "this turn ran past its compute limit";

/// What a turn ended by a malformed reply says.
pub const MALFORMED: &str = "the model's reply was cut off partway through a tool call, \
     so there was nothing it asked for that could be run. Asking again often works; \
     asking for something shorter works more often";

/// The model answered with a tool call nobody can read. See the gateway's
/// `is_malformed_reply`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct MalformedReply(String);

/// The provider declined to answer, in its own sentence. Not retried: the
/// same conversation is declined the same way, and every try is paid for.
#[derive(Debug, thiserror::Error)]
#[error("{}", .0.reason)]
pub struct Refused(pub outturn_core::protocol::work::Refusal);

impl HeldError {
    /// Whether the guest ran out of fuel.
    ///
    /// The one failure a retry is sure to repeat: fuel is spent by executing
    /// the guest, and the same input executes the same way. Searched for down
    /// the chain rather than at the top, because a trap may arrive wrapped in
    /// whatever context the host call that noticed it added.
    pub fn out_of_fuel(&self) -> bool {
        self.error.chain().any(|e| {
            matches!(
                e.downcast_ref::<wasmtime::Trap>(),
                Some(wasmtime::Trap::OutOfFuel)
            )
        })
    }
}

impl std::fmt::Display for HeldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
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
    /// The model the last round was served by, as the gateway said. None when
    /// it said nothing -- never the model the agent asked for, which a route
    /// may have replaced.
    pub model: Option<String>,
}

impl WasiView for AgentHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Requests through the gateway one turn may make, and listings of storage.
///
/// Fixed rather than settings: they are runaway guards, set far above what a
/// turn doing real work reaches, and a model call already bounds how many a
/// sane turn asks for. `chat` rounds have their own limit, which a workspace
/// sets; without these, a turn with unbounded rounds -- allowed, for long work
/// -- had nothing at all between one looping tool call and the next.
pub const MAX_FETCHES_PER_TURN: u32 = 1_000;
pub const MAX_LISTS_PER_TURN: u32 = 1_000;

/// Bytes a turn may write when the API did not say: the setting's default.
pub const DEFAULT_MAX_WRITTEN_BYTES: u64 = 1 << 30;

impl AgentHost {
    /// Counts one more of something against a fixed per-turn limit.
    fn count(used: &mut u32, limit: u32, what: &str) -> Result<(), String> {
        *used += 1;
        if *used > limit {
            return Err(format!("this turn has made {limit} {what}, its limit"));
        }
        Ok(())
    }
}

impl outturn::agent::host::Host for AgentHost {
    async fn chat(&mut self, request: CompletionRequest) -> Result<Completion, String> {
        AgentHost::chat(self, request).await
    }

    async fn fetch(&mut self, request: HttpRequest) -> Result<HttpResponse, String> {
        AgentHost::fetch(self, request).await
    }

    async fn describe_image(&mut self, path: String, question: String) -> Result<String, String> {
        AgentHost::describe_image(self, path, question).await
    }

    async fn read_object(
        &mut self,
        path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        AgentHost::read_object(self, path, offset, len).await
    }

    async fn read_bytes(&mut self, path: String, offset: u64, len: u32) -> Result<Vec<u8>, String> {
        AgentHost::read_bytes(self, path, offset, len).await
    }

    async fn stat_object(&mut self, path: String) -> Result<ObjectInfo, String> {
        AgentHost::stat_object(self, path).await
    }

    async fn write_object(&mut self, path: String, data: Vec<u8>) -> Result<u64, String> {
        AgentHost::write_object(self, path, data).await
    }

    async fn open_writer(
        &mut self,
        path: String,
    ) -> Result<wasmtime::component::Resource<WriterState>, String> {
        AgentHost::open_writer(self, path).await
    }

    async fn open_reader(
        &mut self,
        path: String,
    ) -> Result<wasmtime::component::Resource<ReaderState>, String> {
        AgentHost::open_reader(self, path).await
    }

    async fn render_pdf(
        &mut self,
        markdown: String,
        destination: String,
    ) -> Result<ObjectInfo, String> {
        AgentHost::render_pdf(self, markdown, destination).await
    }

    async fn delete_object(&mut self, path: String) -> Result<(), String> {
        AgentHost::delete_object(self, path).await
    }

    async fn list_objects(&mut self, prefix: String) -> Result<Vec<ObjectInfo>, String> {
        AgentHost::list_objects(self, prefix).await
    }

    async fn tool_finished(&mut self, outcome: ToolOutcome) {
        if let Some(sink) = &self.on_tool_result {
            sink(&outcome);
        }
    }

    async fn pending_input(&mut self) -> Vec<Arrival> {
        let (arrivals, ids): (Vec<Arrival>, Vec<Option<uuid::Uuid>>) =
            std::mem::take(&mut self.arrivals).into_iter().unzip();
        // Reported here rather than when the gateway handed them over: this
        // is the round boundary the guest chose, so it is where the reply
        // actually changed course.
        let ids: Vec<uuid::Uuid> = ids.into_iter().flatten().collect();
        if let (Some(sink), false) = (&self.on_absorbed, ids.is_empty()) {
            sink(&ids);
        }
        arrivals
    }

    async fn sleep(&mut self, seconds: u64, reason: String) -> Result<String, String> {
        AgentHost::sleep(self, seconds, reason).await
    }

    async fn set_timer(&mut self, due: TimerDue, reason: String) -> Result<String, String> {
        AgentHost::set_timer(self, due, reason).await
    }

    async fn list_timers(&mut self) -> Result<String, String> {
        AgentHost::list_timers(self).await
    }

    async fn cancel_timer(&mut self, id: String) -> Result<String, String> {
        AgentHost::cancel_timer(self, id).await
    }

    async fn current_limits(&mut self) -> Limits {
        Limits {
            max_tool_rounds: self.max_tool_rounds,
            cancelled: self.cancelled,
        }
    }

    async fn eager_tools(&mut self) -> Vec<String> {
        self.eager_tools.clone()
    }

    async fn skill_operations(&mut self) -> Vec<SkillOperation> {
        self.skill_operations
            .iter()
            .map(|o| SkillOperation {
                name: o.name.clone(),
                description: o.description.clone(),
                file: o.file.clone(),
                method: o.method.clone(),
                url: o.url.clone(),
                body: o.body.clone(),
                query: o.query.clone(),
                schema: o.schema.clone(),
            })
            .collect()
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
        AgentHost::current_time(self).await
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

use outturn_core::protocol::work::SignedReply as Signed;
