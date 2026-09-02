mod store;

pub use store::{
    Job, JobError, JobHandle, claim, complete, enqueue, extend_lease, fail, reap_abandoned,
};

use std::time::Duration;

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

/// How often a worker extends the lease on work it is still running.
///
/// Must be comfortably shorter than DEFAULT_LEASE so a single slow renewal
/// does not let the lease lapse under a job that is still running.
pub const LEASE_HEARTBEAT: Duration = Duration::from_secs(15);
