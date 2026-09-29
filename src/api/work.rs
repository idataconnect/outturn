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

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::Authority;
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
    /// When `gateway_token` stops being good, so the runtime knows when to ask
    /// for another rather than learning it from a refused call. Taken just
    /// before minting, so it is never later than the token's own expiry.
    pub gateway_token_expires_at: chrono::DateTime<chrono::Utc>,
    /// The claim this turn was handed out under. Quoted back when the turn
    /// is reported or handed back, so a pod whose lease lapsed cannot write
    /// over the pod that now holds it.
    pub lease_token: Uuid,
}

/// Header a runtime quotes its lease in.
pub const LEASE_HEADER: &str = "x-outturn-lease";

pub(super) fn lease_from(headers: &axum::http::HeaderMap) -> Result<Uuid, ApiError> {
    headers
        .get(LEASE_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or((
            StatusCode::BAD_REQUEST,
            format!("{LEASE_HEADER} must carry the lease this turn was handed out under"),
        ))
}

/// Hands out one turn, or nothing.
///
/// Claims inside the request rather than before it, so a turn is only ever
/// claimed on behalf of a pod that is asking for it right now.
pub async fn take(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Option<Assignment>>, ApiError> {
    // WorkTake rather than GatewayInvoke: the latter is held by workspace Admins
    // and Operators, and a turn handed out carries whichever workspace's
    // transcript it belongs to. Anything that can ask for work can ask for
    // everyone's, so this has to be an authority no workspace role holds.
    super::router::authorize(&state, &headers, Authority::WorkTake).await?;

    let deadline = tokio::time::Instant::now() + WORK_POLL_TIMEOUT;
    loop {
        let claimed = jobs::claim(
            &state.pool,
            &[super::worker::CHAT_TURN],
            1,
            jobs::DEFAULT_LEASE,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if let Some(handle) = claimed.into_iter().next() {
            let payload: super::worker::ChatTurnPayload =
                serde_json::from_value(handle.job.payload.clone())
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("payload: {e}")))?;

            // Released rather than dropped. A claimed job whose handler
            // returns early is a row left `running` that nothing will ever
            // report, and a serial key admits no second job while one is
            // running -- so dropping one here wedges that session until a
            // reaper notices, or for ever if none does.
            let Some(lease_token) = handle.job.lease_token else {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "a claim came back without a lease".to_string(),
                ));
            };

            let Some(worker) = state.worker.get() else {
                let _ = jobs::release(
                    &state.pool,
                    handle.job.id,
                    Duration::from_secs(0),
                    jobs::MAX_RELEASES,
                    Some(lease_token),
                )
                .await;
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "this pod is not serving turns".to_string(),
                ));
            };

            match worker.prepare_turn(handle.job.id, &payload).await {
                // Nothing left to do: the prompt was answered inside the turn
                // it interrupted. The job is done rather than abandoned, and
                // the runtime is not troubled with it.
                Ok(crate::api::worker::Prepared::Nothing) => {
                    let _ =
                        jobs::complete(&state.pool, handle.job.id, handle.job.lease_token).await;
                    continue;
                }
                // A hold refused it, and it is being kept rather than
                // finished. Parked rather than released: a release counts
                // towards MAX_RELEASES and fails the job past it, which is
                // right for "no pod had room" and wrong for a wait that may
                // last days. Parked rather than completed, which is what the
                // suspended verdict used to do -- announcing a resumption with
                // nothing left to resume.
                //
                // The lease goes with it, so the reaper leaves it alone. What
                // gives it back is `resume_parked`, when somebody releases the
                // hold.
                Ok(crate::api::worker::Prepared::Park) => {
                    let _ = jobs::park(&state.pool, handle.job.id, handle.job.lease_token).await;
                    continue;
                }
                Ok(crate::api::worker::Prepared::Run(request)) => {
                    let request = *request;
                    let gateway_token_expires_at = token_expiry();
                    let gateway_token = match mint_for(
                        &state,
                        payload.session_id,
                        payload.workspace_id,
                        request.egress_commitment,
                        request.gate_commitment,
                    ) {
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
                                Some(lease_token),
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
                        gateway_token_expires_at,
                        lease_token,
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
                        handle.job.lease_token,
                    )
                    .await;
                    // Nothing was announced for this turn, so the browser
                    // would otherwise wait on a reply that is never coming.
                    // The last attempt is when it becomes final.
                    if handle.job.attempts >= handle.job.max_attempts {
                        let _ = crate::events::append(
                            &state.pool,
                            payload.workspace_id,
                            Some(payload.session_id),
                            "chat.error",
                            serde_json::json!({
                                "message": e.to_string(),
                                "message_id": payload.message_id,
                            }),
                        )
                        .await;
                    }
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
/// runtime happens to hold. `egress_commitment` comes from the same
/// `ExecuteRequest` this token is minted for, so the claim and the rules
/// travelling beside it are always the API's word about the same turn.
pub fn mint_for(
    state: &ApiState,
    session_id: Uuid,
    workspace_id: Uuid,
    egress_commitment: crate::egress::commit::Hash,
    gate_commitment: crate::egress::commit::Hash,
) -> Result<String, ApiError> {
    state
        .minter
        .mint_turn(session_id, workspace_id, egress_commitment, gate_commitment)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// When a token minted now will stop being good, erring early.
///
/// Read before minting, so the runtime is told a moment no later than the
/// token's real expiry and refreshes a moment early rather than late.
fn token_expiry() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::seconds(crate::auth::SERVICE_TOKEN_LIFETIME_SECS as i64)
}

#[derive(Debug, serde::Deserialize)]
pub struct RefreshToken {
    /// The token the runtime holds now, still good. What the new one may say
    /// is read from it, so a reissued token can claim nothing its predecessor
    /// did not.
    pub gateway_token: String,
}

#[derive(Debug, Serialize)]
pub struct Refreshed {
    pub gateway_token: String,
    pub gateway_token_expires_at: chrono::DateTime<chrono::Utc>,
}

/// Reissues a running turn's gateway token before it runs out.
///
/// A turn has no upper bound while it is working -- onboarding in long-horizon
/// mode can run for hours -- and its token has to, so the runtime trades one in
/// once it is within `TURN_TOKEN_REFRESH_BELOW_SECS` of expiring.
///
/// The new token commits to exactly what the old one did. The commitments are
/// copied out of the presented token rather than recomputed from the
/// workspace's rules now: the runtime is carrying the rules the turn started
/// with, and a token committing to a different set would fail every request
/// it made. The same reason means removing a host mid-turn does not reach into
/// a turn already running -- which is what a five-minute token also did, for
/// five minutes.
///
/// Only a token that is still good is accepted. An expired one is refused
/// rather than read, because a path that honours an expired credential is the
/// kind of exception that outlives the reason it was added.
pub async fn refresh_token(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(job_id): axum::extract::Path<Uuid>,
    Json(input): Json<RefreshToken>,
) -> Result<Json<Refreshed>, ApiError> {
    // The runtime tier and the lease, as every other report requires: the
    // shared key says only "a runtime", the lease says which turn is its.
    super::router::authorize(&state, &headers, Authority::WorkTake).await?;
    let job = jobs::get(&state.pool, job_id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "no such turn".to_string()))?;
    let lease = lease_from(&headers)?;
    if job.lease_token != Some(lease) {
        return Err((
            StatusCode::CONFLICT,
            "that turn is not leased to this runtime".to_string(),
        ));
    }

    let claims = state
        .auth
        .for_audience(crate::auth::AUDIENCE_GATEWAY)
        .validate(&input.gateway_token)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;

    // The token must be this turn's. Without the check a runtime holding two
    // turns could trade one tenant's token in under another's lease.
    let payload: super::worker::ChatTurnPayload = serde_json::from_value(job.payload.clone())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("payload: {e}")))?;
    if claims.subject != payload.session_id || claims.workspace_id != payload.workspace_id {
        return Err((
            StatusCode::FORBIDDEN,
            "that token is not for this turn".to_string(),
        ));
    }

    // Both required. A turn token without them is refused by the gateway
    // anyway, and reissuing one would be minting a claim nobody made.
    let (Some(egress), Some(gates)) = (claims.egress_commitment, claims.gate_commitment) else {
        return Err((
            StatusCode::FORBIDDEN,
            "that token carries no commitments to reissue".to_string(),
        ));
    };

    let gateway_token_expires_at = token_expiry();
    let gateway_token = mint_for(
        &state,
        payload.session_id,
        payload.workspace_id,
        egress,
        gates,
    )?;
    Ok(Json(Refreshed {
        gateway_token,
        gateway_token_expires_at,
    }))
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
    super::router::authorize(&state, &headers, Authority::WorkTake).await?;

    let worker = state.worker.get().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "this pod is not serving turns".to_string(),
    ))?;

    // Only the pod holding the current lease may report. The claim is the
    // ticket: a job that is pending was never handed out, one that already
    // succeeded has been reported once, and one whose lease lapsed is out
    // with somebody else. Without this, holding a job id is enough to write
    // into a transcript -- including over a turn another pod is streaming.
    let lease = lease_from(&headers)?;
    let held = jobs::holds_lease(&state.pool, job_id, lease)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !held {
        return Err((
            StatusCode::CONFLICT,
            "that turn is not out with this runtime".to_string(),
        ));
    }

    worker
        .finish_turn(job_id, lease, body.into_data_stream())
        .await
        .map_err(|e| {
            // A lease that lapsed mid-report is the same refusal as one that
            // had lapsed before it started, not a fault in this tier.
            if e.downcast_ref::<super::worker::LeaseLost>().is_some() {
                (
                    StatusCode::CONFLICT,
                    "that turn is no longer leased to this runtime".to_string(),
                )
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })?;

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
    super::router::authorize(&state, &headers, Authority::WorkTake).await?;
    let lease = lease_from(&headers)?;

    // Given back rather than failed: nothing about the turn was wrong, the
    // pod running it could not report what it produced.
    match jobs::release(
        &state.pool,
        job_id,
        Duration::from_secs(1),
        jobs::MAX_RELEASES,
        Some(lease),
    )
    .await
    {
        Ok(jobs::Released::Queued) => {
            tracing::info!(job_id = %job_id, "a runtime handed a turn back");
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
