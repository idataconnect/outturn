//! Handing work to runtimes that ask for it.
//!
//! Turns used to be pushed: a worker claimed a job and posted it to whichever
//! runtime the Service happened to pick, which knew nothing about which pods
//! had room. A full pod answered 503, the turn was handed back, and two
//! seconds later the same guess was made again. It worked -- nothing was lost
//! and nothing failed -- but a measured run of a hundred and twenty turns
//! spent six hundred and ninety-two refusals finding room, and the turns that
//! kept losing that lottery waited fourteen seconds before generating a single
//! token while others started in a tenth of a second.
//!
//! The unfairness was the problem rather than the waiting. Which turn drew the
//! short straw was luck, and no amount of retrying faster changes that.
//!
//! So a runtime asks. A pod with a free slot polls for a turn and is given
//! one; a pod with no room does not ask, and is never offered work it would
//! have to refuse. Capacity stops being something the API guesses at from the
//! outside and becomes something each pod states by asking.
//!
//! One turn per request, deliberately. Handing a pod several would mean
//! guessing again at what it can hold -- the cost of a turn is not known until
//! it runs -- and asking for one is the same statement of capacity that
//! admission control used to make by refusing.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::{Authority, Role};
use crate::jobs;

use super::router::{ApiError, ApiState};

/// How long a runtime's request for work waits before coming back empty.
///
/// Matches the browser's event poll: long enough that an idle cluster is not
/// constantly reconnecting, short enough that a pod shutting down is not held
/// open waiting for a turn that will never come.
const WORK_POLL_TIMEOUT: Duration = Duration::from_secs(25);

/// How often the queue is re-examined while a runtime waits.
///
/// Polled rather than notified. The events feed can listen on a Postgres
/// channel because an event is a thing that happened; a job is a thing to be
/// taken, and a notification that woke every idle runtime at once would have
/// them race for one turn and all but one lose. A short poll spreads that out
/// and costs one indexed query per pod per interval.
const WORK_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// A turn handed to a runtime, with everything it needs to run it.
#[derive(Debug, Serialize)]
pub struct Assignment {
    pub job_id: Uuid,
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub agent_id: Uuid,
    /// The prompt this turn answers, so the reply can hang off it.
    pub message_id: Uuid,
    /// Attempts already spent, so a runtime can tell a retry from a first go.
    pub attempts: i32,
    pub max_attempts: i32,
}

/// Hands out one turn, or nothing.
///
/// Claims inside the request rather than before it, so a turn is only ever
/// claimed on behalf of a pod that is asking for it right now.
pub async fn take(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Option<Assignment>>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    // The same authority the runtime already holds to reach the gateway: this
    // is the platform's own tier asking for work, not a tenant's.
    claims
        .require(Authority::GatewayInvoke)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    let deadline = tokio::time::Instant::now() + WORK_POLL_TIMEOUT;
    loop {
        let claimed = jobs::claim(&state.pool, &[super::worker::CHAT_TURN], 1, jobs::DEFAULT_LEASE)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if let Some(handle) = claimed.into_iter().next() {
            let payload: super::worker::ChatTurnPayload =
                serde_json::from_value(handle.job.payload.clone()).map_err(|e| {
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("payload: {e}"))
                })?;

            tracing::debug!(
                job_id = %handle.job.id,
                session_id = %payload.session_id,
                "handed a turn to a runtime that asked for one"
            );

            return Ok(Json(Some(Assignment {
                job_id: handle.job.id,
                session_id: payload.session_id,
                tenant_id: payload.tenant_id,
                agent_id: payload.agent_id,
                message_id: payload.message_id,
                attempts: handle.job.attempts,
                max_attempts: handle.job.max_attempts,
            })));
        }

        if tokio::time::Instant::now() >= deadline {
            // Empty rather than an error: there was no work, which is the
            // ordinary state of an idle cluster.
            return Ok(Json(None));
        }

        tokio::select! {
            _ = tokio::time::sleep(WORK_POLL_INTERVAL) => {}
            // A shutting-down API must not hold a runtime's request open past
            // its own exit, or the pod waits out the full timeout for nothing.
            _ = Arc::clone(&state.shutdown).notified_owned() => {
                return Ok(Json(None));
            }
        }
    }
}

/// Mints the token a runtime's turn travels with.
///
/// Minted here rather than held by the runtime, so what a turn may reach is
/// bounded by what this tier granted for that turn rather than by whatever the
/// runtime happens to hold.
pub fn mint_for(state: &ApiState, session_id: Uuid, tenant_id: Uuid) -> Result<String, ApiError> {
    state
        .minter
        .mint(session_id, tenant_id, &[Role::Operator])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
