//! A circuit breaker shared by every gateway replica.
//!
//! An in-process breaker does not help here. Each replica would discover an
//! outage separately and keep its own probe traffic flowing, so a provider
//! that is down still receives one storm per pod. The state lives in Postgres
//! so backing off is a collective decision.
//!
//! **It fails open.** Every path that cannot reach the database proceeds as
//! though the circuit were closed. Refusing to call a model because the
//! transcript store is unreachable would turn a database blip into a total
//! loss of inference -- a worse outage than the one being guarded against.
//!
//! **It assumes tightly synchronised clocks.** Every timestamp here is
//! wall-clock, because it is the only time two replicas can both name. Pods
//! whose clocks differ by more than the backoff will disagree about when a
//! probe is due; the cost is an early probe rather than a wrong answer, since
//! the claim is a conditional UPDATE and only one replica wins it. Run NTP.
//!
//! What to open a circuit *about* is in `policy`, which is pure and knows
//! nothing of this table -- see its docs for why a 500 from one caller is not
//! the same evidence as a 500 from five.

pub mod policy;

use sqlx::Row;
use sqlx::postgres::PgPool;

/// Whether a call may be attempted.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The circuit is closed, or this replica has claimed the probe.
    Allow,
    /// The circuit is open and someone else owns the next probe.
    Reject,
}

/// What one provider error is evidence of.
///
/// The three kinds are the point of `policy`, and this is where a provider's
/// vocabulary is translated into them. `Unavailable` is the endpoint not being
/// there at all, so one caller reporting it is enough. A 5xx means it answered
/// and failed, which one caller cannot tell apart from its own bad request --
/// that needs breadth before it means anything. A 4xx and a rate limit are the
/// provider working correctly: the first says the request was wrong, the
/// second says it is alive and applying backpressure, and neither is evidence
/// about health.
pub fn observation_for(error: &super::llm::provider::ProviderError) -> policy::Observation {
    use super::llm::provider::ProviderError::*;
    match error {
        Unavailable => policy::Observation::Unreachable,
        e if e.is_client_error() => policy::Observation::NotEvidence,
        // A transport error arrives here too, carrying no status. It is the
        // endpoint failing rather than answering, but one caller's truncated
        // stream is not yet an outage, so it waits for company.
        Upstream { .. } => policy::Observation::Undetermined,
        RateLimited | Translation(_) => policy::Observation::NotEvidence,
    }
}

/// Which circuit an endpoint's health is kept under.
///
/// A workspace bringing its own credential to a host does not share a fate
/// with anyone else reaching it, so it gets its own circuit. The platform's own
/// credential is shared, and so is the rate limit behind it, so one circuit
/// covers everybody.
pub fn scope_of(workspace_id: Option<uuid::Uuid>) -> policy::Scope {
    match workspace_id {
        Some(_) => policy::Scope::Workspace,
        None => policy::Scope::Platform,
    }
}

/// Reads a circuit, including the evidence that has not aged out.
async fn load(
    pool: &PgPool,
    endpoint: &str,
    workspace_id: Option<uuid::Uuid>,
) -> Result<policy::Health, sqlx::Error> {
    let row = sqlx::query(
        "select state, failures, probe_after, opened_at from provider_health          where endpoint = $1 and workspace_id is not distinct from $2",
    )
    .bind(endpoint)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await?;

    let mut health = policy::Health::new(scope_of(workspace_id));
    if let Some(row) = row {
        let state: String = row.get("state");
        health.state = match state.as_str() {
            "open" => policy::State::Open,
            "half_open" => policy::State::HalfOpen,
            _ => policy::State::Closed,
        };
        let failures: i32 = row.get("failures");
        health.failures = failures.max(0) as u32;
        health.probe_after = row.get("probe_after");
        health.opened_at = row.get("opened_at");
    }

    // Only what is still inside the window: breadth has to be breadth now, and
    // a query that read everything would make an old incident count forever.
    let sightings = sqlx::query(
        "select seen_by_workspace, seen_by_session, seen_at from breaker_sightings          where endpoint = $1 and workspace_id is not distinct from $2            and seen_at > now() - make_interval(secs => $3)",
    )
    .bind(endpoint)
    .bind(workspace_id)
    .bind(policy::BREADTH_WINDOW.as_secs_f64())
    .fetch_all(pool)
    .await?;

    health.sightings = sightings
        .iter()
        .map(|row| policy::Sighting {
            caller: policy::Caller {
                workspace_id: row.get("seen_by_workspace"),
                session_id: row.get("seen_by_session"),
            },
            at: row.get("seen_at"),
        })
        .collect();

    Ok(health)
}

/// Writes back what the policy decided.
async fn store(
    pool: &PgPool,
    endpoint: &str,
    workspace_id: Option<uuid::Uuid>,
    health: &policy::Health,
    last_error: Option<&str>,
) -> Result<(), sqlx::Error> {
    let state = match health.state {
        policy::State::Closed => "closed",
        policy::State::Open => "open",
        policy::State::HalfOpen => "half_open",
    };

    // Two partial uniques, so two conflict targets: `on conflict` has to name
    // an index that actually covers the row being written, and the platform's
    // rows are exactly the ones a predicate on `workspace_id is not null`
    // excludes. One query for both would silently insert a duplicate for every
    // platform write -- failures would never accumulate, because each one
    // would land on a row of its own.
    let sql = if workspace_id.is_some() {
        "insert into provider_health \
             (id, endpoint, workspace_id, state, failures, probe_after, opened_at, last_error) \
         values (uuidv7(), $1, $2, $3, $4, $5, $6, $7) \
         on conflict (endpoint, workspace_id) where workspace_id is not null do update \
         set state = $3, failures = $4, probe_after = $5, opened_at = $6, \
             last_error = $7, updated_at = now()"
    } else {
        "insert into provider_health \
             (id, endpoint, workspace_id, state, failures, probe_after, opened_at, last_error) \
         values (uuidv7(), $1, $2, $3, $4, $5, $6, $7) \
         on conflict (endpoint) where workspace_id is null do update \
         set state = $3, failures = $4, probe_after = $5, opened_at = $6, \
             last_error = $7, updated_at = now()"
    };

    sqlx::query(sql)
        .bind(endpoint)
        .bind(workspace_id)
        .bind(state)
        .bind(health.failures as i32)
        .bind(health.probe_after)
        .bind(health.opened_at)
        .bind(last_error)
        .execute(pool)
        .await?;

    Ok(())
}

/// Decides whether `endpoint` may be called, claiming the probe if one is due.
///
/// The policy decides; this claims. A probe is offered to whoever asks once it
/// is due, and the claim is a single conditional update, so exactly one replica
/// wins it -- the losers see no affected row and keep rejecting. That is also
/// what makes a replica dying mid-probe recoverable, and what keeps two
/// replicas with drifting clocks to one wasted probe rather than a storm.
pub async fn check(pool: &PgPool, endpoint: &str, workspace_id: Option<uuid::Uuid>) -> Verdict {
    // Fail open throughout: an unreachable database says nothing about the
    // endpoint, and refusing every call because the store is down would turn a
    // database blip into the outage it was meant to prevent.
    let Ok(health) = load(pool, endpoint, workspace_id).await else {
        return Verdict::Allow;
    };

    match policy::check(&health, chrono::Utc::now()) {
        policy::Verdict::Allow => Verdict::Allow,
        policy::Verdict::Reject => Verdict::Reject,
        policy::Verdict::Probe => {
            let claimed = sqlx::query(
                "update provider_health \
                 set state = 'half_open', \
                     probe_after = now() + make_interval(secs => $3), \
                     updated_at = now() \
                 where endpoint = $1 and workspace_id is not distinct from $2 \
                   and probe_after is not null \
                   and probe_after <= now() \
                 returning endpoint",
            )
            .bind(endpoint)
            .bind(workspace_id)
            .bind(policy::PROBE_LEASE.as_secs_f64())
            .fetch_optional(pool)
            .await;

            match claimed {
                Ok(Some(_)) => Verdict::Allow,
                Ok(None) => Verdict::Reject,
                Err(_) => Verdict::Allow,
            }
        }
    }
}

/// Records what one call showed about an endpoint.
///
/// Takes the caller because breadth is counted over callers rather than over
/// reports: what makes an answered failure mean something is how many distinct
/// callers are seeing it, and a count of reports is exactly the measure that
/// let one noisy agent close a provider for everybody.
pub async fn observe(
    pool: &PgPool,
    endpoint: &str,
    workspace_id: Option<uuid::Uuid>,
    observation: policy::Observation,
    caller: policy::Caller,
    detail: Option<&str>,
) {
    // Nothing is learned and nothing is written. Kept ahead of the read so an
    // endpoint answering 404s all day costs no queries at all.
    if observation == policy::Observation::NotEvidence {
        return;
    }

    let now = chrono::Utc::now();

    // The sighting is written first, so the read that follows includes it and
    // two replicas seeing the same thing at once both count.
    if observation == policy::Observation::Undetermined {
        let written = sqlx::query(
            "insert into breaker_sightings \
                 (id, endpoint, workspace_id, seen_by_workspace, seen_by_session) \
             values (uuidv7(), $1, $2, $3, $4)",
        )
        .bind(endpoint)
        .bind(workspace_id)
        .bind(caller.workspace_id)
        .bind(caller.session_id)
        .execute(pool)
        .await;
        if let Err(e) = written {
            tracing::warn!(endpoint, error = %e, "could not record a sighting");
            return;
        }
    }

    let Ok(health) = load(pool, endpoint, workspace_id).await else {
        tracing::warn!(endpoint, "could not read circuit health");
        return;
    };

    let was = health.state;
    let next = policy::record(&health, observation, caller, now);

    if let Err(e) = store(pool, endpoint, workspace_id, &next, detail).await {
        tracing::warn!(endpoint, error = %e, "could not record what a call showed");
        return;
    }

    if was != policy::State::Open && next.state == policy::State::Open {
        // Worth a line at warn: somebody reading logs during an incident wants
        // to know when this stopped being one caller's problem.
        let distinct = next
            .sightings
            .iter()
            .map(|s| s.caller.workspace_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        tracing::warn!(
            endpoint,
            failures = next.failures,
            distinct_callers = distinct,
            "circuit opened"
        );
    }
}

/// Deletes evidence too old to be evidence of anything current.
///
/// Nothing else would ever remove these rows, and a table that only grows is
/// one somebody meets at 3am. Called on whatever schedule the caller likes;
/// deleting nothing is not an error.
pub async fn forget_stale_sightings(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let deleted = sqlx::query(
        "delete from breaker_sightings where seen_at < now() - make_interval(secs => $1)",
    )
    .bind(policy::BREADTH_WINDOW.as_secs_f64() * 10.0)
    .execute(pool)
    .await?;
    Ok(deleted.rows_affected())
}
