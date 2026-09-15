//! What the breaker decides, with nothing it decides it against.
//!
//! The breaker's state lives in Postgres because backing off is a collective
//! decision, and its concurrency -- who claims the next probe -- is settled by
//! a conditional UPDATE. What is here is the part in between: given what has
//! been observed about an endpoint, should the next call go out. No database,
//! no clock, no sockets. `now` arrives as an argument, so a test can hand this
//! a timeline rather than wait for one.
//!
//! The reason for pulling it out is an incident shape this codebase should not
//! repeat. One agent sending a request that makes a provider's upstream throw
//! 500s tripped a breaker for everybody, because the breaker was counting
//! failures and the failures were all about one caller's request rather than
//! about the service. Counting harder does not fix that; counting a different
//! dimension does.
//!
//! So evidence falls into three kinds, and only one of them is about the
//! service on its own:
//!
//! - **Unambiguous.** A connect timeout, a DNS failure, a reset connection, a
//!   503. Nothing about one caller's request explains these, so one caller
//!   reporting them is enough.
//! - **Undetermined until sampled.** A 500, 502 or 504. The service answered,
//!   and it failed, but whether it failed *at this request* or *at everything*
//!   is not knowable from one caller. It is not weak evidence to be discounted:
//!   it is evidence whose meaning needs a wider sample. Once enough distinct
//!   callers report it, it is the strongest signal there is -- the service is
//!   up, answering, and failing, which is when hammering it helps least.
//! - **Not evidence.** A 400, 401, 403, 404. The service answered correctly
//!   and the request was wrong. A workspace with an expired credential must
//!   never be able to open a breaker for tenants whose credentials are fine.
//!
//! What stops one noisy caller is that breadth counts *distinct callers*, so
//! volume from one of them cannot impersonate many. An undetermined failure
//! also never holds an open circuit open or delays a probe; it contributes to
//! breadth and to nothing else, or the misattribution would simply have moved.
//!
//! **Clocks must be tightly synchronised.** Every time here is wall-clock,
//! because it is the only time two replicas can both name; `Instant` is
//! monotonic but process-local and cannot be stored or compared across pods.
//! Replicas whose clocks differ by more than the backoff will disagree about
//! when a probe is due, which costs an early probe rather than correctness --
//! the claim is conditional, so only one replica wins it. Durations derived
//! from stored timestamps are clamped, so a clock stepped backwards reads as
//! zero elapsed rather than as a negative or absurd interval.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Consecutive unambiguous failures before the circuit opens.
///
/// Kept from the breaker this policy was extracted from: more than one,
/// because a single error is often a blip; low enough that a dead endpoint is
/// not called many times per replica before anyone notices.
pub const FAILURE_THRESHOLD: u32 = 5;

/// How many *distinct callers* must report undetermined failures before they
/// mean anything about the service.
///
/// Three rather than two: two callers can share a cause that is not the
/// service, such as one workspace's agents running the same broken skill.
pub const BREADTH_THRESHOLD: usize = 3;

/// How long undetermined evidence counts for.
///
/// Breadth has to be breadth *now*. Without ageing, a caller from this morning
/// and two from this afternoon would look like an outage that never happened.
pub const BREADTH_WINDOW: Duration = Duration::from_secs(120);

/// How long the circuit stays open before a probe is allowed.
pub const BASE_BACKOFF: Duration = Duration::from_secs(30);

/// Ceiling on the backoff, so a long outage does not push the next probe
/// beyond the point where anyone is still waiting for recovery.
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// How long a claimed probe has to finish before another replica may try.
pub const PROBE_LEASE: Duration = Duration::from_secs(60);

/// What one call told us about an endpoint.
///
/// Named for what it is evidence *of*, not for the status code that produced
/// it, so the classification is made once where the protocol is understood
/// rather than re-derived by everything that looks at a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// The call worked.
    Success,
    /// The endpoint could not be reached or is plainly unwell: a connect
    /// timeout, a DNS failure, a reset, a 503. One caller is enough.
    Unreachable,
    /// The endpoint answered and failed, and one caller cannot tell whether
    /// that is about the service or about its own request: a 500, 502, 504.
    Undetermined,
    /// The endpoint answered correctly and the request was wrong: a 4xx, or
    /// rate limiting, which means it is alive and applying backpressure.
    /// Never evidence about health.
    NotEvidence,
}

/// Which circuit an observation belongs to.
///
/// A breaker is only meaningful over callers who share a fate. Two workspaces
/// with their own credentials against one host do not: one expired key would
/// otherwise open the circuit for the other. So the scope follows the
/// credential -- the platform's own credential gets a platform-wide circuit,
/// and a workspace's own gets a circuit of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One circuit for everybody, for an endpoint reached with the platform's
    /// credential and subject to its shared rate limit.
    Platform,
    /// One circuit per workspace, which is the default: a workspace that has
    /// not said otherwise does not get to speak for anyone else.
    Workspace,
}

impl Scope {
    /// The unit breadth is counted in.
    ///
    /// A platform circuit counts workspaces, because that is the boundary a
    /// shared credential and a shared rate limit sit on, and counting sessions
    /// would let one busy workspace reach breadth alone. A workspace circuit
    /// counts sessions, for the same reason one level down.
    pub fn caller_unit(self) -> CallerUnit {
        match self {
            Scope::Platform => CallerUnit::Workspace,
            Scope::Workspace => CallerUnit::Session,
        }
    }
}

/// What counts as "a distinct caller" for breadth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerUnit {
    Workspace,
    Session,
}

/// Where an observation came from, so breadth can be counted over callers
/// rather than over requests.
///
/// The finer half is whatever the caller could prove it was. A turn token names
/// the workspace and the chat session, so a session is what stands for "one
/// caller" below the workspace -- not an agent id, which the gateway is never
/// told and would have to be taken on trust from the tier that must not be
/// believed about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Caller {
    pub workspace_id: uuid::Uuid,
    pub session_id: uuid::Uuid,
}

impl Caller {
    fn key(&self, unit: CallerUnit) -> uuid::Uuid {
        match unit {
            CallerUnit::Workspace => self.workspace_id,
            CallerUnit::Session => self.session_id,
        }
    }
}

/// The circuit's condition, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Open,
    /// A probe has been claimed and has not reported back.
    HalfOpen,
}

/// One caller's undetermined failure, remembered only long enough to be part
/// of a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighting {
    pub caller: Caller,
    pub at: DateTime<Utc>,
}

/// Everything known about one endpoint's circuit.
#[derive(Debug, Clone)]
pub struct Health {
    pub state: State,
    pub scope: Scope,
    /// Consecutive unambiguous failures. Reset by a success.
    pub failures: u32,
    /// When a probe may next be attempted, once open.
    pub probe_after: Option<DateTime<Utc>>,
    /// When the circuit opened, which is what the backoff grows from.
    pub opened_at: Option<DateTime<Utc>>,
    /// Undetermined failures still inside the window, one per report.
    pub sightings: Vec<Sighting>,
}

impl Health {
    /// A circuit nobody has reported anything about.
    pub fn new(scope: Scope) -> Self {
        Self {
            state: State::Closed,
            scope,
            failures: 0,
            probe_after: None,
            opened_at: None,
            sightings: Vec::new(),
        }
    }
}

/// Whether a call may be attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The circuit is closed.
    Allow,
    /// The circuit is open and a probe is due, so this caller may take it.
    /// Whether it actually does is settled by the conditional claim in the
    /// store, not here: two replicas can both be told this and only one wins.
    Probe,
    /// The circuit is open and the next probe is not due yet.
    Reject,
}

/// Elapsed time that cannot be negative or absurd.
///
/// Both ends come from wall clocks that may have been stepped or may belong to
/// different replicas. A negative interval reads as no time passed, and
/// anything beyond the ceiling is capped, so a clock jump costs at worst an
/// early probe rather than a circuit that never reopens.
fn elapsed(now: DateTime<Utc>, since: DateTime<Utc>, ceiling: Duration) -> Duration {
    now.signed_duration_since(since)
        .to_std()
        .unwrap_or(Duration::ZERO)
        .min(ceiling)
}

/// How long to wait before the next probe, given how long this has been down.
///
/// Doubles with each backoff already served rather than with a failure count,
/// so an endpoint nobody is calling does not creep towards the ceiling.
pub fn backoff_for(now: DateTime<Utc>, opened_at: Option<DateTime<Utc>>) -> Duration {
    let Some(opened_at) = opened_at else {
        return BASE_BACKOFF;
    };
    let down = elapsed(now, opened_at, MAX_BACKOFF);
    let doublings = (down.as_secs() / BASE_BACKOFF.as_secs().max(1)) as u32;
    BASE_BACKOFF
        .saturating_mul(1u32 << doublings.min(16))
        .min(MAX_BACKOFF)
}

/// Whether a call to this endpoint may go out.
pub fn check(health: &Health, now: DateTime<Utc>) -> Verdict {
    match health.state {
        State::Closed => Verdict::Allow,
        // A claimed probe holds the circuit for its lease. Without that, every
        // replica arriving while one is in flight would send its own.
        State::HalfOpen => match health.probe_after {
            Some(due) if now >= due => Verdict::Probe,
            Some(_) => Verdict::Reject,
            None => Verdict::Probe,
        },
        State::Open => match health.probe_after {
            Some(due) if now >= due => Verdict::Probe,
            Some(_) => Verdict::Reject,
            // Open with no probe scheduled is a row somebody wrote wrong. A
            // probe is the recoverable reading: the alternative never reopens.
            None => Verdict::Probe,
        },
    }
}

/// What an observation changes about a circuit.
///
/// Returns the health as it should now be stored. The caller writes it; this
/// decides it.
pub fn record(
    health: &Health,
    observation: Observation,
    caller: Caller,
    now: DateTime<Utc>,
) -> Health {
    let mut next = health.clone();
    next.sightings
        .retain(|s| elapsed(now, s.at, BREADTH_WINDOW) < BREADTH_WINDOW);

    match observation {
        // Success closes the circuit however it was doing. A probe that
        // succeeded is the recovery this was all waiting for, and breadth
        // gathered on the way down is spent: it described an outage that has
        // just ended.
        Observation::Success => {
            next.state = State::Closed;
            next.failures = 0;
            next.probe_after = None;
            next.opened_at = None;
            next.sightings.clear();
        }

        // One caller is enough: nothing about a single request explains a
        // connection that was never made.
        Observation::Unreachable => {
            next.failures = next.failures.saturating_add(1);
            if next.state == State::HalfOpen {
                // The probe failed. Back off again from when this last opened,
                // without needing the evidence to be gathered afresh.
                next.state = State::Open;
                next.probe_after = Some(now + backoff(backoff_for(now, next.opened_at)));
            } else if next.failures >= FAILURE_THRESHOLD {
                next.state = State::Open;
                next.opened_at = Some(next.opened_at.unwrap_or(now));
                next.probe_after = Some(now + backoff(backoff_for(now, next.opened_at)));
            }
        }

        // Evidence that means nothing until it is wide. It contributes a
        // sighting and touches nothing else -- not the failure count, not the
        // probe schedule -- so a single caller failing loudly can neither open
        // a circuit nor hold one open.
        Observation::Undetermined => {
            let unit = next.scope.caller_unit();
            next.sightings.push(Sighting { caller, at: now });
            if next.state == State::Closed {
                let distinct: HashSet<uuid::Uuid> =
                    next.sightings.iter().map(|s| s.caller.key(unit)).collect();
                if distinct.len() >= BREADTH_THRESHOLD {
                    next.state = State::Open;
                    next.opened_at = Some(now);
                    next.probe_after = Some(now + backoff(backoff_for(now, Some(now))));
                }
            }
        }

        // The service answered correctly and the request was wrong. It says
        // nothing about health, and a caller with a bad credential must not be
        // able to speak for anyone else.
        Observation::NotEvidence => {}
    }

    next
}

/// `chrono`'s own duration, from the standard one, for adding to a timestamp.
fn backoff(d: Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or_else(|_| chrono::Duration::seconds(30))
}

/// Marks a probe as taken, for the replica that won the claim.
pub fn claim_probe(health: &Health, now: DateTime<Utc>) -> Health {
    let mut next = health.clone();
    next.state = State::HalfOpen;
    next.probe_after = Some(now + backoff(PROBE_LEASE));
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> uuid::Uuid {
        uuid::Uuid::now_v7()
    }

    fn caller() -> Caller {
        Caller {
            workspace_id: ws(),
            session_id: ws(),
        }
    }

    /// Distinct sessions inside one workspace, which is one caller to a
    /// platform circuit and many to a workspace one.
    fn sessions_of(workspace_id: uuid::Uuid, n: usize) -> Vec<Caller> {
        (0..n)
            .map(|_| Caller {
                workspace_id,
                session_id: ws(),
            })
            .collect()
    }

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
    }

    fn after(base: DateTime<Utc>, secs: i64) -> DateTime<Utc> {
        base + chrono::Duration::seconds(secs)
    }

    fn observe(
        health: Health,
        observation: Observation,
        caller: Caller,
        at: DateTime<Utc>,
    ) -> Health {
        record(&health, observation, caller, at)
    }

    #[test]
    fn one_caller_failing_loudly_cannot_open_a_shared_circuit() {
        // The incident this policy exists for: one agent's bad request makes a
        // provider's upstream throw, and everybody's traffic stops.
        let mut health = Health::new(Scope::Platform);
        let noisy = caller();
        for i in 0..1_000 {
            health = observe(health, Observation::Undetermined, noisy, after(t0(), i));
        }
        assert_eq!(health.state, State::Closed);
        assert_eq!(check(&health, after(t0(), 1_000)), Verdict::Allow);
    }

    #[test]
    fn enough_distinct_callers_failing_the_same_way_opens_it() {
        // The same observation from three workspaces is no longer about one
        // caller's request: the service is answering and failing.
        let mut health = Health::new(Scope::Platform);
        for (i, c) in [caller(), caller(), caller()].into_iter().enumerate() {
            health = observe(health, Observation::Undetermined, c, after(t0(), i as i64));
        }
        assert_eq!(health.state, State::Open);
        assert_eq!(check(&health, after(t0(), 3)), Verdict::Reject);
    }

    #[test]
    fn one_caller_alone_opens_it_for_something_only_the_service_explains() {
        // Nothing about one request explains a connection that was never made,
        // so breadth is not wanted here.
        let mut health = Health::new(Scope::Platform);
        let one = caller();
        for i in 0..FAILURE_THRESHOLD {
            health = observe(health, Observation::Unreachable, one, after(t0(), i as i64));
        }
        assert_eq!(health.state, State::Open);
    }

    #[test]
    fn a_workspace_with_a_bad_credential_never_trips_anything() {
        // 401 forever is the service working correctly and the caller being
        // wrong. A tenant who has not rotated a key must not take a host away
        // from tenants whose keys are fine.
        let mut health = Health::new(Scope::Platform);
        for (i, c) in (0..50).map(|i| (i, caller())) {
            health = observe(health, Observation::NotEvidence, c, after(t0(), i));
        }
        assert_eq!(health.state, State::Closed);
        assert!(health.sightings.is_empty());
        assert_eq!(health.failures, 0);
    }

    #[test]
    fn breadth_is_callers_rather_than_reports() {
        // Volume from one caller must not be able to impersonate many, which
        // is the whole mechanism: counting reports would make this identical
        // to the counting that misattributed the incident.
        let mut health = Health::new(Scope::Platform);
        let two = [caller(), caller()];
        for i in 0..100 {
            let c = two[i as usize % 2];
            health = observe(health, Observation::Undetermined, c, after(t0(), i));
        }
        assert_eq!(health.state, State::Closed);
    }

    #[test]
    fn a_platform_circuit_counts_workspaces_and_a_workspace_one_counts_sessions() {
        // Three sessions of one workspace are one caller to the platform, and
        // three to that workspace: a shared credential is what the platform
        // circuit is about, and a busy workspace should not speak for the rest.
        let agents = sessions_of(ws(), 3);

        let mut platform = Health::new(Scope::Platform);
        for (i, c) in agents.iter().enumerate() {
            platform = observe(platform, Observation::Undetermined, *c, after(t0(), i as i64));
        }
        assert_eq!(platform.state, State::Closed);

        let mut workspace = Health::new(Scope::Workspace);
        for (i, c) in agents.iter().enumerate() {
            workspace = observe(
                workspace,
                Observation::Undetermined,
                *c,
                after(t0(), i as i64),
            );
        }
        assert_eq!(workspace.state, State::Open);
    }

    #[test]
    fn evidence_ages_out_so_yesterdays_breadth_does_not_open_today() {
        // Breadth has to be breadth now. Two callers this morning and one this
        // afternoon is not an outage anybody is having.
        let mut health = Health::new(Scope::Platform);
        health = observe(health, Observation::Undetermined, caller(), t0());
        health = observe(health, Observation::Undetermined, caller(), after(t0(), 1));

        let much_later = after(t0(), BREADTH_WINDOW.as_secs() as i64 + 10);
        health = observe(health, Observation::Undetermined, caller(), much_later);

        assert_eq!(health.state, State::Closed);
        assert_eq!(health.sightings.len(), 1, "stale sightings should be gone");
    }

    #[test]
    fn an_undetermined_failure_never_delays_a_probe() {
        // Otherwise the misattribution moves rather than goes: one caller
        // could not open the circuit, but could keep it shut.
        let mut health = Health::new(Scope::Platform);
        let one = caller();
        for i in 0..FAILURE_THRESHOLD {
            health = observe(health, Observation::Unreachable, one, after(t0(), i as i64));
        }
        let scheduled = health.probe_after.expect("open circuits schedule a probe");

        health = observe(health, Observation::Undetermined, caller(), after(t0(), 10));
        assert_eq!(health.probe_after, Some(scheduled));
        assert_eq!(health.failures, FAILURE_THRESHOLD);
    }

    #[test]
    fn a_probe_is_offered_only_once_it_is_due() {
        let mut health = Health::new(Scope::Platform);
        let one = caller();
        for i in 0..FAILURE_THRESHOLD {
            health = observe(health, Observation::Unreachable, one, after(t0(), i as i64));
        }

        let due = health.probe_after.expect("scheduled");
        assert_eq!(check(&health, due - chrono::Duration::seconds(1)), Verdict::Reject);
        assert_eq!(check(&health, due), Verdict::Probe);
    }

    #[test]
    fn a_claimed_probe_holds_the_circuit_for_its_lease() {
        // Every replica arriving while a probe is in flight would otherwise
        // send its own, which is the storm this exists to prevent.
        let mut health = Health::new(Scope::Platform);
        let one = caller();
        for i in 0..FAILURE_THRESHOLD {
            health = observe(health, Observation::Unreachable, one, after(t0(), i as i64));
        }
        let due = health.probe_after.expect("scheduled");

        let claimed = claim_probe(&health, due);
        assert_eq!(claimed.state, State::HalfOpen);
        assert_eq!(check(&claimed, due), Verdict::Reject);
        assert_eq!(
            check(&claimed, due + chrono::Duration::seconds(PROBE_LEASE.as_secs() as i64)),
            Verdict::Probe,
            "a probe that never reported back must not hold the circuit forever"
        );
    }

    #[test]
    fn a_successful_probe_closes_it_and_spends_the_evidence() {
        let mut health = Health::new(Scope::Platform);
        for (i, c) in [caller(), caller(), caller()].into_iter().enumerate() {
            health = observe(health, Observation::Undetermined, c, after(t0(), i as i64));
        }
        let due = health.probe_after.expect("scheduled");
        let health = claim_probe(&health, due);

        let recovered = observe(health, Observation::Success, caller(), due);
        assert_eq!(recovered.state, State::Closed);
        assert_eq!(check(&recovered, due), Verdict::Allow);
        assert!(
            recovered.sightings.is_empty(),
            "breadth described an outage that has ended"
        );
    }

    #[test]
    fn a_failed_probe_reopens_without_gathering_the_evidence_again() {
        let mut health = Health::new(Scope::Platform);
        let one = caller();
        for i in 0..FAILURE_THRESHOLD {
            health = observe(health, Observation::Unreachable, one, after(t0(), i as i64));
        }
        let due = health.probe_after.expect("scheduled");
        let health = claim_probe(&health, due);

        let failed = observe(health, Observation::Unreachable, one, due);
        assert_eq!(failed.state, State::Open);
        assert!(
            failed.probe_after.expect("rescheduled") > due,
            "a failed probe should wait longer, not retry immediately"
        );
    }

    #[test]
    fn a_clock_stepped_backwards_does_not_strand_the_circuit() {
        // Wall clocks can be stepped by NTP and replicas disagree. The reading
        // that matters is that the circuit still reopens: a negative interval
        // must not become an absurd backoff or a probe that never comes due.
        let mut health = Health::new(Scope::Platform);
        let one = caller();
        for i in 0..FAILURE_THRESHOLD {
            health = observe(health, Observation::Unreachable, one, after(t0(), i as i64));
        }

        let backwards = after(t0(), -3_600);
        assert_eq!(
            backoff_for(backwards, health.opened_at),
            BASE_BACKOFF,
            "time before the circuit opened reads as no time served"
        );
        // And far in the future, the ceiling holds.
        assert_eq!(
            backoff_for(after(t0(), 10_000_000), health.opened_at),
            MAX_BACKOFF
        );
    }

    #[test]
    fn a_long_outage_backs_off_towards_the_ceiling_but_not_past_it() {
        let opened = Some(t0());
        assert_eq!(backoff_for(t0(), opened), BASE_BACKOFF);
        assert!(backoff_for(after(t0(), 120), opened) > BASE_BACKOFF);
        assert_eq!(backoff_for(after(t0(), 100_000), opened), MAX_BACKOFF);
    }

    #[test]
    fn nothing_a_single_caller_does_can_open_a_platform_circuit_by_answering() {
        // The property behind the example tests: whatever sequence of answered
        // failures one caller produces, over any timeline, a shared circuit
        // stays closed. Only unreachability, which no request explains, opens
        // it alone.
        let noisy = caller();
        let mut health = Health::new(Scope::Platform);
        let pattern = [
            Observation::Undetermined,
            Observation::NotEvidence,
            Observation::Undetermined,
            Observation::Undetermined,
            Observation::NotEvidence,
        ];
        for i in 0..500i64 {
            let observation = pattern[i as usize % pattern.len()];
            // Timestamps that jump around, including backwards.
            let at = after(t0(), (i * 7) % 300 - 50);
            health = observe(health, observation, noisy, at);
            assert_eq!(
                health.state,
                State::Closed,
                "one caller opened a shared circuit at step {i}"
            );
        }
    }
}
