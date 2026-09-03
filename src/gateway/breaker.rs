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

use std::time::Duration;

use sqlx::postgres::PgPool;
use sqlx::Row;

/// Consecutive failures before the circuit opens.
///
/// More than one, because a single error is often a bad request or a blip
/// rather than an outage; low enough that a genuinely dead provider is not
/// called many times per replica before anyone notices.
const FAILURE_THRESHOLD: i32 = 5;

/// How long the circuit stays open before a probe is allowed.
const BASE_BACKOFF: Duration = Duration::from_secs(30);

/// Ceiling on the backoff, so a long outage does not push the next probe
/// beyond the point where anyone is still waiting for recovery.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// How long a claimed probe has to finish before another replica may try.
const PROBE_LEASE: Duration = Duration::from_secs(60);

/// Whether a call may be attempted.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The circuit is closed, or this replica has claimed the probe.
    Allow,
    /// The circuit is open and someone else owns the next probe.
    Reject,
}

/// Whether an error should count against a provider's health.
///
/// A rejected request is not an outage. A malformed body or an unknown model
/// is our fault and would trip the breaker on every replica for a bug that
/// affects one caller; rate limiting is backpressure, which means the provider
/// is alive and answering. Only unreachability and upstream faults count.
pub fn counts_as_failure(error: &super::llm::provider::ProviderError) -> bool {
    use super::llm::provider::ProviderError::*;
    match error {
        Unavailable => true,
        // A 4xx here is a request we got wrong; a 5xx or a transport error is
        // the provider failing. The distinction is in the message because the
        // variant does not carry a status.
        Upstream(detail) => !starts_with_client_error(detail),
        RateLimited | Translation(_) => false,
    }
}

/// Upstream errors are formatted as `"{status}: {body}"`, so the status is the
/// leading token when there is one at all.
fn starts_with_client_error(detail: &str) -> bool {
    detail
        .split(':')
        .next()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .is_some_and(|status| (400..500).contains(&status) && status != 408 && status != 429)
}

/// Decides whether `endpoint` may be called, claiming the probe if one is due.
///
/// The claim is a single conditional update, so exactly one replica wins it:
/// the losers see no affected row and keep rejecting. Moving `probe_after`
/// forward is what stops a second replica probing while the first is still in
/// flight, and what makes a replica that dies mid-probe recoverable.
pub async fn check(pool: &PgPool, endpoint: &str) -> Verdict {
    let row = sqlx::query("select state, probe_after from provider_health where endpoint = $1")
        .bind(endpoint)
        .fetch_optional(pool)
        .await;

    let Ok(row) = row else {
        // Fail open: an unreachable database says nothing about the provider.
        return Verdict::Allow;
    };

    let Some(row) = row else {
        // Never seen. Nothing has failed, so nothing is open.
        return Verdict::Allow;
    };

    let state: String = row.get("state");
    if state == "closed" {
        return Verdict::Allow;
    }

    let claimed = sqlx::query(
        "update provider_health \
         set state = 'half_open', \
             probe_after = now() + make_interval(secs => $2), \
             updated_at = now() \
         where endpoint = $1 \
           and probe_after is not null \
           and probe_after <= now() \
         returning endpoint",
    )
    .bind(endpoint)
    .bind(PROBE_LEASE.as_secs_f64())
    .fetch_optional(pool)
    .await;

    match claimed {
        Ok(Some(_)) => Verdict::Allow,
        Ok(None) => Verdict::Reject,
        // Fail open rather than reject on a database fault.
        Err(_) => Verdict::Allow,
    }
}

/// Records that a call succeeded, closing the circuit.
pub async fn record_success(pool: &PgPool, endpoint: &str) {
    let result = sqlx::query(
        "insert into provider_health (endpoint, state, failures, probe_after, opened_at) \
         values ($1, 'closed', 0, null, null) \
         on conflict (endpoint) do update \
         set state = 'closed', failures = 0, probe_after = null, \
             opened_at = null, last_error = null, updated_at = now()",
    )
    .bind(endpoint)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::warn!(endpoint, error = %e, "could not record provider success");
    }
}

/// Records a failure, opening the circuit once they accumulate.
///
/// The backoff grows with the failure count rather than resetting each time
/// the circuit reopens, so a provider that fails every probe is called
/// progressively less often instead of every thirty seconds forever.
pub async fn record_failure(pool: &PgPool, endpoint: &str, error: &str) {
    let result = sqlx::query(
        "insert into provider_health (endpoint, state, failures, last_error) \
         values ($1, 'closed', 1, $2) \
         on conflict (endpoint) do update \
         set failures = provider_health.failures + 1, \
             last_error = $2, \
             state = case when provider_health.failures + 1 >= $3 then 'open' \
                          else provider_health.state end, \
             opened_at = case when provider_health.failures + 1 >= $3 \
                              then coalesce(provider_health.opened_at, now()) \
                              else provider_health.opened_at end, \
             probe_after = case when provider_health.failures + 1 >= $3 \
                                then now() + make_interval(secs => least( \
                                    $4 * power(2, provider_health.failures + 1 - $3), $5)) \
                                else provider_health.probe_after end, \
             updated_at = now() \
         returning state, failures",
    )
    .bind(endpoint)
    .bind(error)
    .bind(FAILURE_THRESHOLD)
    .bind(BASE_BACKOFF.as_secs_f64())
    .bind(MAX_BACKOFF.as_secs_f64())
    .fetch_optional(pool)
    .await;

    match result {
        Ok(Some(row)) => {
            let state: String = row.get("state");
            let failures: i32 = row.get("failures");
            if state == "open" {
                tracing::warn!(endpoint, failures, "provider circuit opened");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(endpoint, error = %e, "could not record provider failure"),
    }
}
