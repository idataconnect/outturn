//! Holds on work, and what it takes to lift them.
//!
//! Zero or more inhibitors apply to a turn. Each contributes a strength, the
//! strongest wins, and work proceeds only when none apply. See
//! `docs/inhibitors.md` for why this is a set rather than a flag -- the short
//! version being that a flag cannot say who is holding it, so releasing becomes
//! indistinguishable from overriding.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod postgres;
pub use postgres::PostgresInhibitorStore;

/// What an inhibitor does to the work it covers.
///
/// Ordered, and the ordering is the whole of the join: `Verdict::max` over the
/// set is the answer. Derived rather than written out, so a strength added
/// later cannot be forgotten in a comparison somewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    /// The work may continue later. What a turn waiting on an approval holds.
    Suspended,
    /// This turn is over. What a kill switch holds.
    Stopped,
}

impl Strength {
    pub fn as_str(self) -> &'static str {
        match self {
            Strength::Suspended => "suspended",
            Strength::Stopped => "stopped",
        }
    }
}

impl std::str::FromStr for Strength {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "suspended" => Ok(Strength::Suspended),
            "stopped" => Ok(Strength::Stopped),
            other => Err(format!("not a strength: {other}")),
        }
    }
}

/// What the set says about a piece of work right now.
///
/// `Proceed` is the empty case of the same rule rather than a separate path:
/// nothing holding is the same question answered with nothing in the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Proceed,
    Suspended,
    Stopped,
}

impl From<Strength> for Verdict {
    fn from(s: Strength) -> Self {
        match s {
            Strength::Suspended => Verdict::Suspended,
            Strength::Stopped => Verdict::Stopped,
        }
    }
}

/// Which work an inhibitor covers.
///
/// Cascades the way settings do, and for the same reason: an operator needs a
/// switch no workspace can override, and a workspace needs one that does not
/// require naming every agent. Unlike settings there is no overriding -- the
/// set that applies to a turn is the union of every level above it, and each
/// holder releases its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum Scope {
    /// Everything this deployment runs.
    Platform,
    Workspace { workspace_id: Uuid },
    Agent { workspace_id: Uuid, agent_id: Uuid },
    /// One conversation. Not a level settings has, because a hold on a single
    /// session is the ordinary shape of a turn waiting for somebody.
    Session { workspace_id: Uuid, session_id: Uuid },
}

impl Scope {
    /// The workspace this is about, where there is one.
    pub fn workspace_id(&self) -> Option<Uuid> {
        match self {
            Scope::Platform => None,
            Scope::Workspace { workspace_id }
            | Scope::Agent { workspace_id, .. }
            | Scope::Session { workspace_id, .. } => Some(*workspace_id),
        }
    }
}

/// A hold somebody is keeping on some work.
#[derive(Debug, Clone, Serialize)]
pub struct Inhibitor {
    pub id: Uuid,
    pub scope: Scope,
    pub strength: Strength,
    /// Why, in a person's words. Shown wherever the hold is shown, because
    /// "why is nothing happening" is the question this exists to answer.
    pub reason: String,
    /// What took it: a machine credential's name, or a user's id. Free text
    /// because a spend cap and a person are both legitimate holders and only
    /// one of them has a row in `users`.
    pub held_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// What the set says, and what said it.
///
/// The verdict is derived at every checkpoint and never stored: a turn
/// suspended waiting on an approval, resumed when it arrives, has to
/// re-evaluate everything, because the workspace's kill switch may have come on
/// while it waited. `contributors` is a snapshot for display and audit, not the
/// authority on whether to go on.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub verdict: Verdict,
    /// Every inhibitor that applied, not only the one that won. A turn both
    /// stopped by an org switch and waiting on an approval must not report only
    /// the stop -- that hides a request somebody is still expected to answer.
    pub contributors: Vec<Inhibitor>,
}

impl Decision {
    /// Whether work may start or carry on.
    pub fn proceeds(&self) -> bool {
        self.verdict == Verdict::Proceed
    }

    /// Why, in one line, for a session that is being stopped.
    ///
    /// Every deciding contributor rather than the first: a person shown one
    /// reason releases one hold and finds the session still stopped. Written
    /// here so both checkpoints say the same thing -- the gateway cutting a
    /// stream and turn preparation refusing the next one are the same stop.
    pub fn why(&self) -> String {
        self.deciding()
            .map(|i| i.reason.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The inhibitors of the strength that decided it.
    ///
    /// What a refusal message quotes: the ones that are stopping this, rather
    /// than every hold that happens to exist.
    pub fn deciding(&self) -> impl Iterator<Item = &Inhibitor> {
        let verdict = self.verdict;
        self.contributors
            .iter()
            .filter(move |i| Verdict::from(i.strength) == verdict)
    }
}

/// What the set says about a piece of work.
///
/// A plain function over a slice rather than anything swappable. There is one
/// correct answer and every call path must get it: a join that could differ
/// between callers is a kill switch that works in one place and not another.
pub fn decide(inhibitors: Vec<Inhibitor>) -> Decision {
    let verdict = inhibitors
        .iter()
        .map(|i| Verdict::from(i.strength))
        .max()
        .unwrap_or(Verdict::Proceed);
    Decision { verdict, contributors: inhibitors }
}

/// Taking a hold.
#[derive(Debug, Clone, Deserialize)]
pub struct TakeInhibitor {
    pub scope: Scope,
    pub strength: Strength,
    pub reason: String,
    pub held_by: String,
}

#[derive(Debug, thiserror::Error)]
pub enum InhibitorError {
    #[error("no such inhibitor")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Internal(String),
}

#[async_trait::async_trait]
pub trait InhibitorStore: Send + Sync {
    /// Takes a hold, returning it with the handle that releases it.
    async fn take(&self, input: TakeInhibitor) -> Result<Inhibitor, InhibitorError>;

    /// Lifts one hold. Others on the same work keep applying, which is the
    /// whole reason this is a set: the last holder out decides.
    async fn release(&self, id: Uuid) -> Result<(), InhibitorError>;

    /// Every inhibitor that applies to this work, across every level above it.
    ///
    /// The cascade is resolved here rather than by the caller, so a checkpoint
    /// cannot accidentally ask about one level and believe it has the answer.
    async fn covering(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
        session_id: Uuid,
    ) -> Result<Vec<Inhibitor>, InhibitorError>;

    /// What is held at exactly this scope, for showing and releasing.
    async fn at(&self, scope: Scope) -> Result<Vec<Inhibitor>, InhibitorError>;

    /// One hold, by the handle taking it returned.
    ///
    /// The caller checks what it covers before acting on it: a hold is
    /// authorised by the work it holds, which cannot be known from the id.
    async fn get(&self, id: Uuid) -> Result<Inhibitor, InhibitorError>;

    /// Everything held anywhere in a workspace, including on its agents and
    /// sessions.
    ///
    /// What a panel shows. Distinct from `covering`, which answers "may this
    /// turn run" and therefore includes platform holds that are none of a
    /// workspace's business.
    async fn in_workspace(&self, workspace_id: Uuid) -> Result<Vec<Inhibitor>, InhibitorError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(strength: Strength) -> Inhibitor {
        Inhibitor {
            id: Uuid::now_v7(),
            scope: Scope::Platform,
            strength,
            reason: "because".into(),
            held_by: "test".into(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn nothing_held_proceeds() {
        // The empty case of the same rule, not a special path.
        let decision = decide(vec![]);
        assert_eq!(decision.verdict, Verdict::Proceed);
        assert!(decision.proceeds());
    }

    #[test]
    fn one_hold_decides_alone() {
        assert_eq!(decide(vec![held(Strength::Suspended)]).verdict, Verdict::Suspended);
        assert_eq!(decide(vec![held(Strength::Stopped)]).verdict, Verdict::Stopped);
    }

    #[test]
    fn the_strongest_wins_however_many_are_weaker() {
        let mut set: Vec<Inhibitor> = (0..50).map(|_| held(Strength::Suspended)).collect();
        set.push(held(Strength::Stopped));
        assert_eq!(decide(set).verdict, Verdict::Stopped);
    }

    #[test]
    fn the_order_they_arrive_in_does_not_matter() {
        // A set, not a sequence: whichever way the rows come back, the answer
        // is the same.
        let first = decide(vec![held(Strength::Stopped), held(Strength::Suspended)]).verdict;
        let second = decide(vec![held(Strength::Suspended), held(Strength::Stopped)]).verdict;
        assert_eq!(first, second);
        assert_eq!(first, Verdict::Stopped);
    }

    #[test]
    fn every_contributor_is_kept_not_only_the_winner() {
        // A turn both stopped and awaiting an approval must not report only the
        // stop: that hides a request somebody is still waiting to answer.
        let decision = decide(vec![held(Strength::Stopped), held(Strength::Suspended)]);
        assert_eq!(decision.contributors.len(), 2);
        assert_eq!(decision.deciding().count(), 1);
    }

    #[test]
    fn a_strength_round_trips_through_its_name() {
        for strength in [Strength::Suspended, Strength::Stopped] {
            assert_eq!(strength.as_str().parse::<Strength>().unwrap(), strength);
        }
    }

    #[test]
    fn stopped_outranks_suspended() {
        // The ordering is the join, so it is worth asserting outright rather
        // than only through `decide`.
        assert!(Strength::Stopped > Strength::Suspended);
        assert!(Verdict::Stopped > Verdict::Suspended);
        assert!(Verdict::Suspended > Verdict::Proceed);
    }
}
