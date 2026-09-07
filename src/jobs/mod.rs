mod store;

pub use store::{
    Job, JobError, JobHandle, claim, complete, enqueue, extend_lease, fail, get, holds_lease, is_running, reap_abandoned, release, Released,
};

use std::time::Duration;

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
