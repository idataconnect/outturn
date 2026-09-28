//! What the workspace's skills are actually doing.
//!
//! Four questions, and the useful two are the ones nobody can ask today.
//!
//! *Authoring* -- what was written, and by whom -- is the obvious cut and the
//! weakest: a count of versions written this week looks like activity and
//! rarely changes a decision. It is here because "has anybody touched this
//! lately" is a fair question and the rows already answer it.
//!
//! *Usage* is turns served. A skill's body is paid for on every round of every
//! turn it is bound to, so this is the closest thing to what a skill costs.
//!
//! *Idle* skills are bound to an agent and served no turn in the window. That
//! is dead weight in every prompt, and nothing else surfaces it.
//!
//! *Lagging* skills served turns on a version that is no longer the newest --
//! the "I fixed the skill and the agent still does the old thing" case, which
//! is either a pin doing its job or an edit nobody has picked up, and the
//! reader is the one who can tell which.
//!
//! Computed where the rows are, for the reason `usage` gives: a browser folding
//! these itself would page the whole transcript down the wire to show a count.

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

/// The window's figures, echoed back with the window itself so a reader of the
/// JSON knows what was asked rather than inferring it from the numbers.
#[derive(Debug, Clone, Serialize)]
pub struct SkillStats {
    pub from: chrono::DateTime<chrono::Utc>,
    pub to: chrono::DateTime<chrono::Utc>,
    pub totals: SkillTotals,
    /// Most-used first, capped. A skill that served nothing in the window is
    /// not here -- it is in `idle` if an agent carries it, and nowhere if not.
    pub used: Vec<SkillUse>,
    /// Bound to an agent and served nothing. Alphabetical: there is no
    /// meaningful order to idleness, and a stable one lets a reader find the
    /// same row twice.
    pub idle: Vec<IdleSkill>,
    /// Served turns on something other than the newest version.
    pub lagging: Vec<LaggingSkill>,
    /// Who wrote versions in the window, most first.
    pub authors: Vec<SkillAuthor>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SkillTotals {
    /// Skills the workspace has that are not retired.
    pub skills: i64,
    /// Of those, how many an agent actually carries.
    pub bound: i64,
    /// Versions written in the window.
    pub versions: i64,
    /// Skills first written in the window.
    pub created: i64,
    /// Skills retired in the window.
    pub retired: i64,
    /// Turns any skill served in the window.
    pub turns: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillUse {
    pub skill_id: Uuid,
    pub name: String,
    pub slug: String,
    /// Replies this skill was part of.
    pub turns: i64,
    /// Distinct conversations, which separates one busy session from many.
    pub sessions: i64,
    /// Versions of it written in the window, so a skill being edited while it
    /// is being used is visible as such.
    pub versions: i64,
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IdleSkill {
    pub skill_id: Uuid,
    pub name: String,
    pub slug: String,
    /// How many agents carry it. Idle on one agent is a different thing from
    /// idle on six.
    pub agents: i64,
    /// When it last served a turn, ever -- not just in the window. `None` means
    /// it never has, which is a stronger statement than "not lately".
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LaggingSkill {
    pub skill_id: Uuid,
    pub name: String,
    pub slug: String,
    /// The newest version's ordinal, and the newest one that actually served.
    pub latest: i32,
    pub serving: i32,
    pub turns: i64,
    /// Whether an agent pins this skill to a version. A pin means the lag is
    /// deliberate; its absence means an edit nobody has picked up.
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillAuthor {
    /// Absent for a version written by the platform rather than a person --
    /// an install, a seed -- which is why this is not a name.
    pub user_id: Option<Uuid>,
    pub name: Option<String>,
    pub versions: i64,
    pub skills: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error("skill stats error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait SkillStatsStore: Send + Sync {
    /// One workspace's figures over a closed window.
    ///
    /// Scoped to a workspace rather than offering the platform-wide cut the
    /// usage summary has: skills are workspace-owned and a list of every
    /// workspace's skill names is not a figure, it is their content.
    async fn stats(
        &self,
        workspace_id: Uuid,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
        limit: i64,
    ) -> Result<SkillStats, StatsError>;
}
