//! Compacting a conversation because somebody asked.
//!
//! A conversation keeps the system prompt it was given until it compacts --
//! see docs/prompt-contributors.md -- so this is how a person takes up a skill
//! published since it began without starting over. Queued under the session's
//! own serial key, the same one its turns use, so it can never run beside one:
//! a summary written while a turn was still adding to the conversation would
//! summarize something already out of date.
//!
//! Runs in the API tier, like naming: it reaches a model and writes the
//! transcript, and needs nothing a runtime has.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use uuid::Uuid;

use crate::auth::Authority;
use crate::jobs;

use super::router::{ApiError, ApiState, authorize};
use super::sessions::{Ownership, session_for};
use super::worker::{PromptStatus, Worker};

/// The job kind.
pub const COMPACT: &str = "session.compact";

/// Whether this conversation's system prompt is what would be composed now.
pub async fn status(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<PromptStatus>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    let session = session_for(
        &state,
        &claims,
        id,
        Authority::SessionsRead,
        Ownership::Suffices,
    )
    .await?;
    let worker = worker(&state)?;
    worker
        .prompt_status(claims.workspace_id, id, session.agent_id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Queues a compaction. The same authority and narrowing as sending a
/// message, since it changes what the agent reads from here on.
pub async fn request(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsCreate).await?;
    session_for(
        &state,
        &claims,
        id,
        Authority::SessionsCreate,
        Ownership::Insufficient,
    )
    .await?;
    jobs::enqueue(
        &state.pool,
        claims.workspace_id,
        COMPACT,
        serde_json::json!({ "session_id": id }),
        None,
        Some(&id.to_string()),
        // Somebody pressed it and is watching for the result.
        jobs::PRIORITY_REALTIME,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::ACCEPTED)
}

fn worker(state: &ApiState) -> Result<&Arc<Worker>, ApiError> {
    state
        .worker
        .get()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "not ready".to_string()))
}

/// Claims compactions and does them, until shutdown.
pub fn spawn(pool: sqlx::PgPool, worker: Arc<Worker>, shutdown: Arc<tokio::sync::Notify>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = shutdown.notified() => return,
                _ = ticker.tick() => {
                    let claimed = match jobs::claim(&pool, &[COMPACT], 1, jobs::DEFAULT_LEASE).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(error = %e, "could not claim compaction work");
                            continue;
                        }
                    };
                    for handle in claimed {
                        let job = handle.job;
                        let session = job
                            .payload
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .and_then(|v| v.parse::<Uuid>().ok());
                        let outcome = match session {
                            Some(session) => worker.compact(job.workspace_id, session).await,
                            None => Err(anyhow::anyhow!("job has no session_id")),
                        };
                        let done = match outcome {
                            Ok(()) => jobs::complete(&pool, job.id, job.lease_token).await,
                            Err(e) => {
                                tracing::warn!(job_id = %job.id, error = %e, "compaction failed");
                                jobs::fail(&pool, job.id, &e.to_string(), Duration::from_secs(10), job.lease_token).await
                            }
                        };
                        if let Err(e) = done {
                            tracing::warn!(job_id = %job.id, error = %e, "could not close compaction job");
                        }
                    }
                }
            }
        }
    });
}
