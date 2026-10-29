//! The worker: what prepares a turn for a runtime and records what came back.
//!
//! In four parts, because they change for different reasons. `turn` is a
//! turn's lifecycle, from the holds that may refuse it to the report that
//! closes its job. `projection` rebuilds the conversation a model is shown
//! from what was stored. `compaction` summarizes a conversation that has
//! outgrown its budget, and composes its system prompt again on request.
//! `sweeps` is the tick that returns abandoned work and clears what nothing
//! will read again.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::agent::AgentStore;
use super::chat::ChatStore;

mod compaction;
mod projection;
mod sweeps;
mod turn;

pub use compaction::{PromptStatus, StaleSkill};
pub(crate) use turn::LeaseHeartbeat;
pub use turn::{LeaseLost, assemble_gates, gates_for};

/// Job kind for "the user said something; produce a reply".
pub const CHAT_TURN: &str = "chat.turn";

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatTurnPayload {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
    /// The user message this turn answers. The reply hangs off it, so a
    /// retried turn fills the reply it already created instead of orphaning
    /// it, and two turns running at once in one session cannot collide.
    pub message_id: Uuid,
    /// IANA zone the sender was in, e.g. "Europe/London". Carried on the turn
    /// rather than the account: it is where the user is now, and a laptop that
    /// crosses a border should not keep answering in the zone it left.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Who sent the prompt, for the usage ledger. Absent on turns nobody
    /// sent -- a schedule, a webhook.
    #[serde(default)]
    pub user_id: Option<Uuid>,
}

pub struct Worker {
    pub pool: PgPool,
    pub agents: Arc<dyn AgentStore>,
    /// The operator's agent templates, which a template agent's turn is
    /// prepared from.
    pub templates: Arc<dyn super::agent_template::AgentTemplateStore>,
    /// The instructions an agent is given beside its own prompt.
    pub skills: Arc<dyn super::skill::SkillStore>,
    pub chat: Arc<dyn ChatStore>,
    /// Where every model call is written down, as it is reported.
    pub usage: Arc<dyn super::usage::UsageStore>,
    /// The cascade a turn's knobs come from.
    pub settings: Arc<dyn super::settings::SettingsStore>,
    /// Holds on work: kill switches, and later the approvals a turn waits on.
    pub inhibitors: Arc<dyn super::inhibitor::InhibitorStore>,
    /// Signs the token a summary's model call carries. Optional because the
    /// worker runs without one: a deployment with no gateway configured still
    /// prepares turns, it just cannot summarize.
    pub minter: Option<Arc<outturn_core::auth::TokenMinter>>,
    /// Where to ask for a summary. Absent means no summarizing, and the trim
    /// underneath carries on alone.
    pub gateway_url: Option<String>,
    /// Where skills' reference files are, for the one an invoked skill brings
    /// into context. Absent means the note names the file instead.
    pub storage: Option<Arc<dyn outturn_core::storage::StorageBackend>>,
}

/// What preparing a turn concluded.
///
/// Three outcomes, because a turn that is being kept is neither ready nor
/// finished. Folding `Park` into "nothing to do" is what let a suspension
/// announce `resumable` and then complete the job, leaving nothing to resume.
#[derive(Debug)]
pub(crate) enum Prepared {
    /// Hand this to a runtime.
    Run(Box<outturn_core::protocol::work::ExecuteRequest>),
    /// Complete the job: there is nothing left to answer.
    Nothing,
    /// Keep the job. A hold refused it, and releasing the hold gives it back.
    Park,
}
