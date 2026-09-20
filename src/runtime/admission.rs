//! Whether this pod can take another turn.
//!
//! A component's cost is not knowable before it runs. Fuel bounds how long a
//! guest may execute, but nothing bounds how much memory it asks for, so a pod
//! that accepts work purely because work exists will eventually accept the
//! turn that kills it -- and a pod killed mid-turn loses every other turn it
//! was carrying, not just the one that pushed it over.
//!
//! So admission is refused before it is fatal, on two grounds: a count, which
//! is predictable and cheap, and free memory, which is what actually runs out.
//! A refusal is not an error. It means "not here", and the queue is the right
//! place for work that has nowhere to go yet.

use std::sync::Arc;

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Turns this pod will carry at once before refusing more.
const DEFAULT_MAX_CONCURRENT_TURNS: usize = 8;

/// Memory kept clear of the limit.
///
/// Not a prediction of what one turn costs -- it is the room a turn already
/// running needs in order to grow. Admitting down to the last byte means the
/// next allocation by work already accepted is the one that fails.
const DEFAULT_MEMORY_RESERVE_BYTES: u64 = 256 * 1024 * 1024;

/// Reports how much memory could still be allocated here.
///
/// A trait so the admission rules can be tested against memory conditions
/// that are impractical to arrange for real.
pub trait MemoryProbe: Send + Sync {
    /// Bytes still available before the limit that applies to this process,
    /// or `None` where that cannot be determined.
    fn available_bytes(&self) -> Option<u64>;
}

/// Reads the container's own limit rather than the machine's.
///
/// The host's free memory is the wrong number in Kubernetes: a pod is killed
/// against its cgroup limit while the node still has gigabytes spare. Falls
/// back to the machine only when no limit is set, which is the case when
/// running outside a container.
pub struct CgroupMemory;

impl CgroupMemory {
    fn read_u64(path: &str) -> Option<u64> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// One counter out of a cgroup `memory.stat`, which is `key value` a line.
    ///
    /// Matched on the whole first field rather than a prefix: v1 carries both
    /// `inactive_file` and `total_inactive_file`, and a prefix match would take
    /// whichever came first.
    fn stat(stat: &str, key: &str) -> Option<u64> {
        stat.lines()
            .filter_map(|line| line.split_once(' '))
            .find(|(name, _)| *name == key)
            .and_then(|(_, value)| value.trim().parse().ok())
    }

    /// What is left before the limit, counting cache the kernel would drop
    /// rather than fail an allocation.
    ///
    /// This is the number a pod is killed against: the kernel reclaims file
    /// cache under pressure, so charged-but-reclaimable pages are not memory
    /// anyone is short of. Subtracting them is what kubelet calls the working
    /// set, and using `current` raw instead is the difference between "this pod
    /// is full" and "this pod has read some files".
    fn headroom(limit: u64, current: u64, inactive_file: u64) -> u64 {
        limit.saturating_sub(current.saturating_sub(inactive_file))
    }

    fn cgroup_v2() -> Option<u64> {
        let limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
        // "max" means no limit, so the cgroup has nothing to say here.
        let limit: u64 = limit.trim().parse().ok()?;
        let current = Self::read_u64("/sys/fs/cgroup/memory.current")?;
        let stat = std::fs::read_to_string("/sys/fs/cgroup/memory.stat").unwrap_or_default();
        // A missing counter costs headroom rather than inventing it: better to
        // refuse a turn that would have fitted than to accept one that will
        // not.
        let inactive_file = Self::stat(&stat, "inactive_file").unwrap_or(0);
        Some(Self::headroom(limit, current, inactive_file))
    }

    fn cgroup_v1() -> Option<u64> {
        let limit = Self::read_u64("/sys/fs/cgroup/memory/memory.limit_in_bytes")?;
        // v1 spells "unlimited" as a number near u64::MAX rather than a word.
        if limit >= u64::MAX / 2 {
            return None;
        }
        let current = Self::read_u64("/sys/fs/cgroup/memory/memory.usage_in_bytes")?;
        let stat = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.stat").unwrap_or_default();
        // v1 reports per-cgroup and hierarchical counters side by side; the
        // hierarchical one is what the limit applies to.
        let inactive_file = Self::stat(&stat, "total_inactive_file").unwrap_or(0);
        Some(Self::headroom(limit, current, inactive_file))
    }

    /// MemAvailable rather than MemFree: the kernel's own estimate of what a
    /// new allocation could actually get, which counts reclaimable cache.
    fn host() -> Option<u64> {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

impl MemoryProbe for CgroupMemory {
    fn available_bytes(&self) -> Option<u64> {
        Self::cgroup_v2()
            .or_else(Self::cgroup_v1)
            .or_else(Self::host)
    }
}

/// Why a turn was not accepted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Already carrying as many turns as it will.
    AtCapacity { in_flight: usize, limit: usize },
    /// Accepting would leave too little room for the turns already running.
    LowMemory { available: u64, reserve: u64 },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::AtCapacity { in_flight, limit } => {
                write!(f, "at capacity: {in_flight} turns running, limit {limit}")
            }
            Refusal::LowMemory { available, reserve } => write!(
                f,
                "low memory: {available} bytes available, {reserve} reserved"
            ),
        }
    }
}

/// Held for as long as the turn runs. Dropping it returns the slot.
///
/// Deliberately opaque: the only correct thing to do with it is keep it
/// alive, and the only correct place to drop it is wherever the turn ends.
#[derive(Debug)]
pub struct Permit {
    #[allow(dead_code)]
    slot: OwnedSemaphorePermit,
    /// What this turn is charged, for as long as it runs.
    charged: u64,
    reserved: Arc<AtomicU64>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        // Whatever this turn was charged goes back when it ends, however it
        // ends. A reservation that outlived its turn would shrink the pod's
        // headroom for the life of the process.
        self.reserved.fetch_sub(self.charged, Ordering::SeqCst);
    }
}

/// What a turn is assumed to cost before it has shown what it costs.
///
/// A turn's real cost is not knowable when it is taken: the prompt is not yet
/// assembled, the model has not answered, and no tool has returned anything.
/// Meanwhile a turn accepted a moment ago is still growing -- allocating,
/// filling a cache, reading a file -- so a pod that measures free memory right
/// after taking work sees headroom that is already spoken for.
///
/// So a turn is charged this much the instant it is taken, and the charge is
/// replaced by the real figure once the turn has grown into it. Guessing high
/// costs a little throughput on a pod that could have taken one more; guessing
/// low costs the pod, and every turn on it.
///
/// Held for the life of the turn rather than replaced by a measurement. An
/// earlier version re-measured after a few seconds, but the figure it used --
/// bytes reserved minus bytes free -- was not the turn's cost, and on any pod
/// with headroom it came out at zero, so every turn was charged nothing after
/// five seconds and the assumption protected nothing. A flat charge is
/// coarser and honest. The guest's own memory is capped separately by the
/// sandbox (see GUEST_MEMORY_LIMIT), so this is the host-side cost of a turn
/// -- the prompt, the stream buffers, the compiled instance -- plus room for
/// a guest to use some of what it is allowed.
const ASSUMED_TURN_BYTES: u64 = 100 * 1024 * 1024;

pub struct Admission {
    slots: Arc<Semaphore>,
    limit: usize,
    reserve_bytes: u64,
    assumed_turn_bytes: u64,
    memory: Arc<dyn MemoryProbe>,
    /// Charged for turns taken but not yet measured. Falls as each turn's real
    /// cost becomes apparent and rises again with the next one taken.
    reserved: Arc<AtomicU64>,
}

impl Admission {
    pub fn new(limit: usize, reserve_bytes: u64, memory: Arc<dyn MemoryProbe>) -> Self {
        Self::with_assumption(limit, reserve_bytes, ASSUMED_TURN_BYTES, memory)
    }

    pub fn with_assumption(
        limit: usize,
        reserve_bytes: u64,
        assumed_turn_bytes: u64,
        memory: Arc<dyn MemoryProbe>,
    ) -> Self {
        // A limit of zero would refuse everything forever, which is never what
        // a misconfigured environment variable meant to express.
        let limit = limit.max(1);
        Self {
            slots: Arc::new(Semaphore::new(limit)),
            limit,
            reserve_bytes,
            assumed_turn_bytes,
            memory,
            reserved: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn from_env() -> Self {
        let limit = std::env::var("OUTTURN_MAX_CONCURRENT_TURNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_CONCURRENT_TURNS);
        let reserve = std::env::var("OUTTURN_MEMORY_RESERVE_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MEMORY_RESERVE_BYTES);
        Self::new(limit, reserve, Arc::new(CgroupMemory))
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn in_flight(&self) -> usize {
        self.limit - self.slots.available_permits()
    }

    /// Takes a slot, or says why not. Never waits: a caller that would queue
    /// here is holding a turn this pod cannot serve, and somewhere else can.
    pub fn try_admit(&self) -> Result<Permit, Refusal> {
        let permit =
            Arc::clone(&self.slots)
                .try_acquire_owned()
                .map_err(|_| Refusal::AtCapacity {
                    in_flight: self.limit,
                    limit: self.limit,
                })?;

        // Checked after the slot is taken, so the count below is what memory
        // would be shared with -- and so an idle pod is never refused.
        let in_flight_before = self.limit - self.slots.available_permits() - 1;
        if in_flight_before > 0
            && let Some(available) = self.memory.available_bytes()
        {
            // What is already spoken for by turns taken but not yet grown into
            // their footprint. Without this a pod reads the headroom of work it
            // has already accepted as though it were free, and takes another
            // turn against memory that is on its way to being used.
            let spoken_for = self.reserved.load(Ordering::SeqCst);
            let free = available.saturating_sub(spoken_for);

            // A pod holding no turns must take this one however little memory
            // it reports. Refusing would leave the work with nowhere to go and
            // no running turn whose ending could change the answer.
            if free < self.reserve_bytes + self.assumed_turn_bytes {
                return Err(Refusal::LowMemory {
                    available: free,
                    reserve: self.reserve_bytes + self.assumed_turn_bytes,
                });
            }
        }

        self.reserved
            .fetch_add(self.assumed_turn_bytes, Ordering::SeqCst);

        Ok(Permit {
            slot: permit,
            charged: self.assumed_turn_bytes,
            reserved: Arc::clone(&self.reserved),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Option<u64>);

    impl MemoryProbe for Fixed {
        fn available_bytes(&self) -> Option<u64> {
            self.0
        }
    }

    /// Tests state the per-turn assumption rather than inheriting the
    /// production one, so their small made-up byte counts stay meaningful.
    fn with_memory(limit: usize, reserve: u64, available: Option<u64>) -> Admission {
        Admission::with_assumption(limit, reserve, 0, Arc::new(Fixed(available)))
    }

    fn with_assumption(
        limit: usize,
        reserve: u64,
        assumed: u64,
        available: Option<u64>,
    ) -> Admission {
        Admission::with_assumption(limit, reserve, assumed, Arc::new(Fixed(available)))
    }

    #[test]
    fn a_free_pod_takes_work() {
        let a = with_memory(4, 100, Some(1_000));
        let _turn = a.try_admit().expect("an idle pod refused work");
        assert_eq!(a.in_flight(), 1);
    }

    #[test]
    fn slots_come_back_when_turns_end() {
        let a = with_memory(2, 100, Some(1_000));
        let first = a.try_admit().expect("admitted");
        let _second = a.try_admit().expect("admitted");
        assert_eq!(a.in_flight(), 2);
        assert!(matches!(a.try_admit(), Err(Refusal::AtCapacity { .. })));

        drop(first);
        assert_eq!(a.in_flight(), 1);
        assert!(
            a.try_admit().is_ok(),
            "a finished turn did not free its slot"
        );
    }

    #[test]
    fn a_busy_pod_refuses_rather_than_queues() {
        let a = with_memory(1, 100, Some(1_000));
        let _held = a.try_admit().expect("admitted");
        assert_eq!(
            a.try_admit().expect_err("a full pod admitted a turn"),
            Refusal::AtCapacity {
                in_flight: 1,
                limit: 1
            }
        );
    }

    #[test]
    fn work_is_refused_before_memory_runs_out() {
        let a = with_memory(4, 500, Some(400));
        let _first = a.try_admit().expect("an idle pod always takes the first");
        assert!(matches!(a.try_admit(), Err(Refusal::LowMemory { .. })));
    }

    #[test]
    fn an_idle_pod_takes_work_however_tight_memory_looks() {
        // Otherwise a pod whose baseline sits under the reserve refuses
        // everything forever, and no turn ending can ever change its mind.
        let a = with_memory(4, 10_000, Some(0));
        assert!(a.try_admit().is_ok());
    }

    #[test]
    fn a_refused_turn_does_not_consume_a_slot() {
        let a = with_memory(4, 500, Some(400));
        let _first = a.try_admit().expect("admitted");
        assert!(a.try_admit().is_err());
        assert_eq!(a.in_flight(), 1, "a refusal left a slot held");
    }

    #[test]
    fn cache_the_kernel_would_drop_is_not_counted_as_used() {
        // Without this a pod that has merely read files reports itself full:
        // page cache is charged to the cgroup and stays charged until there is
        // pressure, so `current` climbs to the limit and never comes back.
        let limit = 1024;
        let current = 1000;
        let inactive_file = 900;
        assert_eq!(CgroupMemory::headroom(limit, current, inactive_file), 924);
        assert_eq!(
            CgroupMemory::headroom(limit, current, 0),
            24,
            "counting reclaimable cache as used is what made a pod look full"
        );
    }

    #[test]
    fn a_counter_is_matched_whole_rather_than_by_prefix() {
        // v1 carries both, and a prefix match takes whichever comes first.
        let stat = "inactive_file 100\ntotal_inactive_file 700\nanon 5\n";
        assert_eq!(CgroupMemory::stat(stat, "inactive_file"), Some(100));
        assert_eq!(CgroupMemory::stat(stat, "total_inactive_file"), Some(700));
        assert_eq!(CgroupMemory::stat(stat, "nope"), None);
    }

    #[test]
    fn headroom_never_wraps() {
        // A cgroup can report usage above its own limit, and a reclaimable
        // figure larger than usage; neither may turn into a huge headroom.
        assert_eq!(CgroupMemory::headroom(100, 200, 0), 0);
        assert_eq!(CgroupMemory::headroom(100, 10, 999), 100);
    }

    #[test]
    fn memory_that_cannot_be_measured_does_not_block_work() {
        let a = with_memory(4, u64::MAX, None);
        let _first = a.try_admit().expect("admitted");
        assert!(a.try_admit().is_ok(), "an unreadable probe refused work");
    }

    /// A turn is charged before it has cost anything.
    ///
    /// Its real footprint is not knowable when it is taken -- no prompt
    /// assembled, no answer, no tool result -- and a turn accepted a moment ago
    /// is still growing into memory the probe has not seen used yet.
    #[test]
    fn a_turn_is_charged_the_moment_it_is_taken() {
        // Room for two assumed turns beside the reserve, and no more.
        let a = with_assumption(8, 100, 400, Some(1000));
        let _first = a.try_admit().expect("an idle pod takes the first");
        let _second = a.try_admit().expect("room for a second");
        assert!(
            matches!(a.try_admit(), Err(Refusal::LowMemory { .. })),
            "a third was taken against memory the first two have not finished \
             claiming"
        );
    }

    #[test]
    fn a_charge_does_not_outlive_its_turn() {
        // A reservation left behind would shrink the pod for the life of the
        // process, one turn at a time.
        let a = with_assumption(8, 100, 400, Some(1000));
        {
            let _first = a.try_admit().expect("admitted");
            let _second = a.try_admit().expect("admitted");
            assert!(a.try_admit().is_err(), "expected to be full");
        }
        assert!(
            a.try_admit().is_ok(),
            "the charge for a finished turn was never given back"
        );
    }
}
