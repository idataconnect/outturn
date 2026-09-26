//! The action queue: what is waiting for a person to do something about it.
//!
//! Distinct from the events feed, which is a log of what happened. An event is
//! true for ever and is read from a cursor; an action item is true until
//! somebody settles it, and is read as a set. Conflating them means either
//! giving the log a state machine or giving the queue a cursor, and both are
//! worse than two tables.
//!
//! Materialised, not derived. The alternative -- computing "what needs me" per
//! reader per page load by joining pending items against current role
//! membership -- pays for the answer on every read, and the badge is read by
//! everyone all the time. Here a change is written once by whoever caused it.
//!
//! What that costs is invalidation: anything which changes the answer has to
//! say so, and a path that forgets leaves a queue that is quietly wrong. The
//! shape of this module is the answer to that. [`Delivery::Targeted`] is the
//! ordinary case -- a caller that knows what changed names it, and the write
//! is proportional to the change. [`Delivery::Invalidate`] recomputes a whole
//! target's queue and exists for the callers that genuinely cannot work it out
//! -- a role's authorities changing, a restore, a reconciliation sweep. It is
//! the fallback rather than the mechanism, because a system that invalidates
//! by default is one where nobody ever finds out which path was lying.

mod postgres;
#[cfg(test)]
mod tests;

pub use postgres::{CHANNEL, PostgresActionStore};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Who an item is waiting on.
///
/// A role rather than the people in it, because membership outlives the
/// moment: expanding a role to its members at write time freezes a snapshot,
/// and every later change to the role is a change the queue never hears about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Target {
    Role(Uuid),
    User(Uuid),
}

impl Target {
    pub fn role_id(&self) -> Option<Uuid> {
        match self {
            Self::Role(id) => Some(*id),
            Self::User(_) => None,
        }
    }

    pub fn user_id(&self) -> Option<Uuid> {
        match self {
            Self::User(id) => Some(*id),
            Self::Role(_) => None,
        }
    }
}

/// How a change reaches the queue.
///
/// Targeted is the ordinary case and invalidation is the last resort. Kept as
/// one enum rather than two methods so a caller has to say which it is, and so
/// the cheap path is the one that is easier to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// These exact items changed for these exact targets. Proportional to the
    /// change.
    Targeted {
        item_ids: Vec<Uuid>,
        targets: Vec<Target>,
    },
    /// Something changed that this call cannot map to items -- a role's
    /// authorities, a bulk import, a reconciliation. Recomputes the target's
    /// whole queue.
    ///
    /// Correct but unbounded: the cost is the size of the target's queue, and
    /// a caller reaching for this on a hot path has usually mistaken not
    /// wanting to work out the answer for not being able to.
    Invalidate { targets: Vec<Target> },
}

impl Delivery {
    /// The targets this delivery concerns, whichever kind it is.
    pub fn targets(&self) -> &[Target] {
        match self {
            Self::Targeted { targets, .. } => targets,
            Self::Invalidate { targets } => targets,
        }
    }

    /// Whether this delivery would do nothing.
    ///
    /// A targeted delivery naming no items, or either kind naming no targets,
    /// is a no-op -- and a no-op that reaches the database is a transaction,
    /// a NOTIFY and a woken client for nothing. Checked here rather than at
    /// each call site so the answer is the same everywhere.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Targeted { item_ids, targets } => item_ids.is_empty() || targets.is_empty(),
            Self::Invalidate { targets } => targets.is_empty(),
        }
    }
}

/// An item raised and waiting.
#[derive(Debug, Clone, Serialize)]
pub struct ActionItem {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub event_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub state: State,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Resolved,
    Cancelled,
    Expired,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    /// Parses what the database holds.
    ///
    /// The column has a check constraint, so an unknown value here means the
    /// constraint and this enum have drifted -- which is a bug rather than bad
    /// input, and `None` lets the caller say so rather than guess.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "resolved" => Some(Self::Resolved),
            "cancelled" => Some(Self::Cancelled),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }

    /// Whether an item in this state is still waiting on somebody.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// What to raise.
#[derive(Debug, Clone)]
pub struct NewItem {
    pub kind: String,
    pub event_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub targets: Vec<Target>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("action item not found")]
    NotFound,
    #[error("action item is already {0}")]
    NotPending(&'static str),
    #[error("invalid action item: {0}")]
    Invalid(String),
    #[error("action store error: {0}")]
    Internal(String),
}

/// Checks an item before it is written.
///
/// Pure, and separate from the store, so the rules can be tested without a
/// database and so both the direct raise and the event-driven classifier get
/// the same answer.
pub fn validate(item: &NewItem) -> Result<(), ActionError> {
    if item.kind.trim().is_empty() {
        return Err(ActionError::Invalid("kind cannot be empty".into()));
    }
    // An item nobody is waiting on is not a queue entry. It would insert
    // cleanly, count towards nobody's badge and never be settled, which is a
    // leak that only shows up as a table that grows.
    if item.targets.is_empty() {
        return Err(ActionError::Invalid(
            "an action item must have at least one target".into(),
        ));
    }
    if !item.payload.is_object() {
        return Err(ActionError::Invalid("payload must be a JSON object".into()));
    }
    Ok(())
}

/// Removes repeats from a target list, keeping the order they were given in.
///
/// The same person can arrive twice -- named directly and again through a role
/// they hold -- and the two are not the same target, so this only collapses
/// exact repeats. The unique indexes would refuse the duplicate anyway; doing
/// it here means a caller assembling a list from two sources is not writing
/// error handling for something that is not an error.
pub fn dedupe_targets(targets: &[Target]) -> Vec<Target> {
    let mut seen = std::collections::HashSet::new();
    targets
        .iter()
        .filter(|t| seen.insert(**t))
        .copied()
        .collect()
}

#[async_trait]
pub trait ActionStore: Send + Sync {
    /// Raises an item and puts it in its targets' queues.
    ///
    /// Takes the workspace explicitly rather than reading it from the item so
    /// that a caller cannot raise into a workspace it did not mean to.
    async fn raise(&self, workspace_id: Uuid, item: NewItem) -> Result<Uuid, ActionError>;

    /// Settles an item.
    ///
    /// Refuses one that is not pending rather than overwriting the settlement,
    /// so two people answering the same request at once produces one winner
    /// and one `NotPending` rather than a silently lost decision.
    async fn settle(
        &self,
        workspace_id: Uuid,
        item_id: Uuid,
        state: State,
        resolved_by: Option<Uuid>,
    ) -> Result<(), ActionError>;

    /// Adds targets to an item that already exists.
    ///
    /// Escalation: a request nobody has picked up widens to a second role.
    /// Idempotent -- a target already there is left alone.
    async fn add_targets(
        &self,
        workspace_id: Uuid,
        item_id: Uuid,
        targets: &[Target],
    ) -> Result<(), ActionError>;

    /// Takes targets off an item without settling it.
    ///
    /// The item stays open for whoever else is targeted. Removing the last
    /// target leaves an item nobody is waiting on, which `orphaned` finds --
    /// it is not refused here because the caller removing a role may be
    /// mid-way through moving it to another.
    async fn remove_targets(
        &self,
        workspace_id: Uuid,
        item_id: Uuid,
        targets: &[Target],
    ) -> Result<(), ActionError>;

    /// What is waiting on this user: items targeted at them directly, plus
    /// items targeted at any role they currently hold.
    ///
    /// Role membership is read here rather than stored per user, so a change
    /// in membership is reflected without touching a single queue row.
    async fn queue_for_user(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<ActionItem>, ActionError>;

    /// What is waiting on this role.
    async fn queue_for_role(
        &self,
        workspace_id: Uuid,
        role_id: Uuid,
    ) -> Result<Vec<ActionItem>, ActionError>;

    /// How many open items are waiting on this user. The badge.
    async fn count_for_user(&self, workspace_id: Uuid, user_id: Uuid) -> Result<i64, ActionError>;

    /// What is waiting on this user across every workspace they belong to.
    ///
    /// The notification centre is global: a decision owed in a workspace the
    /// reader is not currently looking at is exactly the one that would
    /// otherwise go unseen, and a badge per workspace is a badge nobody adds
    /// up. So this is the one read here with no `workspace_id` predicate.
    ///
    /// Which makes it the one place a missing filter leaks another tenant's
    /// work. The workspace set is derived from the reader's role grants and is
    /// never supplied by the caller -- there is deliberately no argument for
    /// it, so no handler can widen it by passing the wrong thing. Losing the
    /// last role in a workspace withdraws that workspace whole, including
    /// items that named the reader directly: membership is the tenancy
    /// boundary, not the targeting.
    /// Bounded by `limit`, oldest first: the queue is read as a screen, and a
    /// reader with a thousand items owed is not served by all of them.
    async fn queue_for_user_everywhere(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ActionItem>, ActionError>;

    /// The global badge. See `queue_for_user_everywhere` for the tenancy rule.
    ///
    /// Bounded by `cap`: a number nobody reads past a point, and an unbounded
    /// count is a scan whose cost grows with somebody else's backlog. The
    /// caller renders `cap` as "and more" rather than as a total.
    async fn count_for_user_everywhere(&self, user_id: Uuid, cap: i64) -> Result<i64, ActionError>;

    /// Announces a change so connected clients refetch.
    ///
    /// Separate from the writes above rather than folded into them: a caller
    /// inside a transaction must not announce until it commits, and only the
    /// caller knows when that is. The writes above that take their own
    /// connection announce for themselves.
    async fn deliver(&self, workspace_id: Uuid, delivery: Delivery) -> Result<(), ActionError>;

    /// Settles items whose expiry has passed. Returns how many.
    ///
    /// Swept rather than filtered on read: an expired item must leave the
    /// badge count, and a count that filters on expiry cannot use the partial
    /// index that makes it cheap.
    async fn sweep_expired(&self, limit: i64) -> Result<u64, ActionError>;

    /// Open items with no targets left.
    ///
    /// Reported rather than cleaned up automatically. An item here is waiting
    /// on nobody, which is a bug in whoever removed the last target -- and
    /// deleting it quietly would remove the evidence.
    async fn orphaned(
        &self,
        workspace_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ActionItem>, ActionError>;
}
