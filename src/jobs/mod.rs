mod store;

pub use store::{Job, JobError, JobHandle, claim, complete, enqueue, fail, reap_abandoned};

use std::time::Duration;

/// How long a claimed job stays leased before the reaper considers it
/// abandoned. Long enough that a slow worker is not stolen from, short enough
/// that a crashed one is retried promptly.
pub const DEFAULT_LEASE: Duration = Duration::from_secs(60);
