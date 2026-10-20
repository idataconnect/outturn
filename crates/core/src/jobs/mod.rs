mod store;

pub use store::{
    Cancelled, Job, JobError, JobHandle, Parked, Released, Requeued, cancel_for_session_on,
    cancel_requested, claim, complete, complete_on, enqueue, extend_lease, fail, fail_for_good,
    get, holds_lease, is_running, live_turn_for_session, mark_cancelled, mark_cancelled_on, park,
    park_on, reap_abandoned, release, request_cancel, requeue_failed, resume_for_scope,
    resume_parked, turn_for_message,
};

use std::time::Duration;

/// What a job's `state` column may hold.
///
/// The schema's CHECK constraint, the partial indexes and the views still
/// enumerate these by hand, because an applied migration cannot be changed
/// -- but no Rust writes the literals any more. A query names one through
/// `quoted()` or a set through `sql_in`, so a state added here reaches every
/// list at once; the one that is missing from a list is the bug this exists
/// to prevent (docs/invariants.md). `ALL` is checked against the constraint
/// in the queue suite, so a state cannot be added on one side only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Pending,
    Running,
    Succeeded,
    Failed,
    /// British, because it is stored: the constraint and the applied
    /// migrations spell it this way, and the browser switches on the text.
    Cancelled,
    /// Waiting on a person. Not running, and not over: the one state expected
    /// to sit that way for hours.
    Parked,
}

impl JobState {
    /// Every state, in the order the constraint lists them.
    pub const ALL: [JobState; 6] = [
        JobState::Pending,
        JobState::Running,
        JobState::Succeeded,
        JobState::Failed,
        JobState::Cancelled,
        JobState::Parked,
    ];

    /// A turn that is not over: queued, running, or waiting on a person. What
    /// a session shows as live, what a stop looks for, and what a trigger's
    /// concurrency ceiling counts. Rendered, this is the predicate of
    /// `jobs_live_turn_workspace_idx` (0023) and `live_turn` (0025) word for
    /// word, which is the only way the planner will use them.
    pub const LIVE: [JobState; 3] = [JobState::Pending, JobState::Running, JobState::Parked];

    pub const fn as_str(self) -> &'static str {
        match self {
            JobState::Pending => "pending",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
            JobState::Parked => "parked",
        }
    }

    /// The state as a SQL literal, quotes included, for a query that names a
    /// state in its text. In the text rather than bound, deliberately: the
    /// planner matches a partial index's predicate against a literal and not
    /// against a parameter, and most of these queries live on one.
    pub const fn quoted(self) -> &'static str {
        match self {
            JobState::Pending => "'pending'",
            JobState::Running => "'running'",
            JobState::Succeeded => "'succeeded'",
            JobState::Failed => "'failed'",
            JobState::Cancelled => "'cancelled'",
            JobState::Parked => "'parked'",
        }
    }

    /// Whether the job is over, one way or another.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        )
    }
}

/// `states` as a SQL list, `('pending', 'running', 'parked')`, for a query's
/// `state in ...`. Spacing matters where the text must equal an index
/// predicate's, which is why it is rendered in one place.
pub fn sql_in(states: &[JobState]) -> String {
    let mut out = String::from("(");
    for (i, state) in states.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(state.quoted());
    }
    out.push(')');
    out
}

impl std::fmt::Display for JobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A state the database holds that this build does not know. Possible only
/// across a deploy that added one, and loud rather than mapped to a default,
/// because every list this enum feeds would be wrong about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownJobState(pub String);

impl std::fmt::Display for UnknownJobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown job state {:?}", self.0)
    }
}

impl std::error::Error for UnknownJobState {}

impl std::str::FromStr for JobState {
    type Err = UnknownJobState;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        JobState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| UnknownJobState(s.to_string()))
    }
}

impl sqlx::Type<sqlx::Postgres> for JobState {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <str as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for JobState {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let text = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(text.parse()?)
    }
}

/// Work somebody is waiting for. Taken before anything else.
pub const PRIORITY_REALTIME: i32 = 10;

/// Work that only has to happen eventually. The default, because most work is.
pub const PRIORITY_BACKGROUND: i32 = 100;

/// How long a claimed job stays leased before the reaper considers it
/// abandoned.
///
/// Deliberately short: a live worker renews this every LEASE_HEARTBEAT while
/// it runs, so the lease no longer has to accommodate the slowest possible
/// execution. Keeping it short is what bounds how long a job sits unreachable
/// after a worker actually dies -- a long lease would leave a crashed worker's
/// job stranded for its full duration, since the heartbeat dies with the
/// process that was writing it.
pub const DEFAULT_LEASE: Duration = Duration::from_secs(45);

/// How many times a job may be handed back for want of room before it is
/// treated as failed.
///
/// At the worker's two-second no-room backoff this is about five minutes of
/// trying, which outlasts a scale-out -- the scaler polls every ten seconds
/// and a new pod is ready inside a minute. Past that, the cluster is not busy
/// but broken, and a user waiting on a reply is better told so than left
/// watching an indicator that will never resolve.
pub const MAX_RELEASES: i32 = 150;

/// How often a worker extends the lease on work it is still running.
///
/// Must be comfortably shorter than DEFAULT_LEASE so a single slow renewal
/// does not let the lease lapse under a job that is still running.
pub const LEASE_HEARTBEAT: Duration = Duration::from_secs(15);

#[cfg(test)]
mod state_tests {
    use super::{JobState, sql_in};

    #[test]
    fn every_state_round_trips_through_its_text() {
        for state in JobState::ALL {
            assert_eq!(state.as_str().parse::<JobState>(), Ok(state));
            assert_eq!(state.quoted(), format!("'{state}'"));
        }
        assert!("done".parse::<JobState>().is_err());
    }

    /// The rendered live set is the applied migrations' predicate, byte for
    /// byte: the planner will not use a partial index whose predicate differs
    /// from the query's by so much as a space, and the migrations cannot be
    /// edited to follow a change here.
    #[test]
    fn the_live_set_renders_as_the_index_predicate() {
        let predicate = format!("state in {}", sql_in(&JobState::LIVE));
        assert_eq!(predicate, "state in ('pending', 'running', 'parked')");
        for migration in [
            include_str!("../../../../migrations/0023_agent_activity.sql"),
            include_str!("../../../../migrations/0025_live_turn.sql"),
        ] {
            assert!(
                migration.contains(&predicate),
                "a migration's predicate no longer matches what queries render"
            );
        }
    }

    #[test]
    fn a_list_of_one_still_has_its_parentheses() {
        assert_eq!(sql_in(&[JobState::Failed]), "('failed')");
    }
}
