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
///
/// Fat on purpose. Preparing a turn -- the agent, the transcript, the egress
/// rules, the reply it will stream into -- touches the database at every step,
/// so it happens here and the runtime is handed the finished article. The
/// alternative, a job id the runtime calls back to expand, costs a round trip
/// to move work to a tier that cannot do it.
#[derive(Debug, Serialize)]
pub struct Assignment {
    pub job_id: Uuid,
    /// Exactly what used to be POSTed to a runtime, travelling the other way.
    #[serde(flatten)]
    pub request: crate::runtime::router::ExecuteRequest,
    /// Minted per turn rather than held by the runtime, so what a turn may
    /// reach is bounded by what this tier granted for it.
    pub gateway_token: String,
}

/// Hands out one turn, or nothing.
///
/// Claims inside the request rather than before it, so a turn is only ever
/// claimed on behalf of a pod that is asking for it right now.
pub async fn take(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Option<Assignment>>, ApiError> {
    // The same authority the runtime already holds to reach the gateway: this
    // is the platform's own tier asking for work, not a tenant's.
    super::router::authorize(&state, &headers, Authority::GatewayInvoke)?;

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

            // Released rather than dropped. A claimed job whose handler
            // returns early is a row left `running` that nothing will ever
            // report, and a serial key admits no second job while one is
            // running -- so dropping one here wedges that session until a
            // reaper notices, or for ever if none does.
            let Some(worker) = state.worker.get() else {
                let _ = jobs::release(
                    &state.pool,
                    handle.job.id,
                    Duration::from_secs(0),
                    jobs::MAX_RELEASES,
                )
                .await;
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "this pod is not serving turns".to_string(),
                ));
            };

            match worker.prepare_turn(&payload).await {
                // Nothing left to do: the prompt was answered inside the turn
                // it interrupted. The job is done rather than abandoned, and
                // the runtime is not troubled with it.
                Ok(None) => {
                    let _ = jobs::complete(&state.pool, handle.job.id).await;
                    continue;
                }
                Ok(Some(request)) => {
                    let gateway_token =
                        match mint_for(&state, payload.session_id, payload.tenant_id) {
                            Ok(token) => token,
                            Err(e) => {
                                // The placeholder is already written and
                                // announced, so this turn has to go back where
                                // a retry can find it rather than be dropped.
                                let _ = jobs::release(
                                    &state.pool,
                                    handle.job.id,
                                    Duration::from_secs(1),
                                    jobs::MAX_RELEASES,
                                )
                                .await;
                                return Err(e);
                            }
                        };
                    tracing::debug!(
                        job_id = %handle.job.id,
                        session_id = %payload.session_id,
                        "handed a turn to a runtime that asked for one"
                    );
                    return Ok(Json(Some(Assignment {
                        job_id: handle.job.id,
                        request,
                        gateway_token,
                    })));
                }
                Err(e) => {
                    // Preparation failed, so nothing ran. Fail the job here
                    // rather than handing a runtime work it cannot do.
                    tracing::error!(job_id = %handle.job.id, error = %e, "could not prepare a turn");
                    let _ = jobs::fail(
                        &state.pool,
                        handle.job.id,
                        &e.to_string(),
                        Duration::from_secs(5),
                    )
                    .await;
                    continue;
                }
            }
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

/// Receives a turn's progress from the runtime that is running it.
///
/// The body is the same newline-delimited stream a runtime used to return when
/// this tier called it; only the direction has changed. Everything that
/// touches the transcript still happens here, so a runtime holds no database
/// and writes no rows -- it produces events and this tier records them.
pub async fn report(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(job_id): axum::extract::Path<Uuid>,
    body: axum::body::Body,
) -> Result<StatusCode, ApiError> {
    super::router::authorize(&state, &headers, Authority::GatewayInvoke)?;

    let worker = state.worker.get().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "this pod is not serving turns".to_string(),
    ))?;

    // Only a turn that is actually out with a runtime may be reported. The
    // claim is the ticket: a job that is pending was never handed out, and one
    // that already succeeded has been reported once. Without this, holding a
    // job id is enough to write into a transcript -- including a second time,
    // over a reply that was already finished.
    let running = jobs::is_running(&state.pool, job_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !running {
        return Err((
            StatusCode::CONFLICT,
            "that turn is not out with a runtime".to_string(),
        ));
    }

    worker
        .finish_turn(job_id, body.into_data_stream())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

/// Hands a turn back when the runtime running it could not deliver the result.
///
/// A runtime that fails to report has no way to say so through the reporting
/// endpoint -- that is the thing that failed -- so it says so here. Without
/// it the only thing that notices is the lease, and a session is blocked for
/// as long as that takes.
pub async fn abandon(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(job_id): axum::extract::Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = super::router::authorize(&state, &headers, Authority::GatewayInvoke)?;

    // Given back rather than failed: nothing about the turn was wrong, the
    // pod running it could not report what it produced.
    match jobs::release(&state.pool, job_id, Duration::from_secs(1), jobs::MAX_RELEASES).await {
        Ok(jobs::Released::Queued) => {
            tracing::info!(job_id = %job_id, actor = %claims.session_id, "a runtime handed a turn back");
        }
        Ok(jobs::Released::GaveUp) => {
            tracing::error!(job_id = %job_id, "a turn was handed back too many times; giving up");
        }
        // Already reported, already reaped, or never ours. Nothing to do, and
        // an error here would only make a runtime retry something that is no
        // longer its business.
        Err(_) => {}
    }

    Ok(StatusCode::NO_CONTENT)
}
