//! The record of declared writes, kept for the runtime host so a write sent
//! twice is one write. See `docs/idempotency.md`.
//!
//! The host derives the key from what the operation declares and asks here
//! before it sends: a write never seen is claimed as `attempted` and sent; one
//! seen answered is answered again from the record without going out; one sent
//! whose answer never came back is refused, because whether it landed is not
//! something anybody here knows.
//!
//! Spoken to by the runtime, like `/v1/work/gated`, and held to the same two
//! checks: the runtime tier's authority, and the lease of the turn it names.
//! The workspace is the job's, never the runtime's word.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use sqlx::Row;
use uuid::Uuid;

use outturn_core::auth::Authority;

use super::router::{ApiError, ApiState};

#[derive(Debug, serde::Deserialize)]
pub struct Claim {
    pub job_id: Uuid,
    pub key: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Claimed {
    /// Never seen, now claimed: send it.
    New,
    /// Sent and answered; this is the answer.
    Completed {
        response: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
    },
    /// Sent, and the answer never seen. Not sent again.
    Attempted { at: chrono::DateTime<chrono::Utc> },
}

#[derive(Debug, serde::Deserialize)]
pub struct Settle {
    pub job_id: Uuid,
    pub key: String,
    /// The recipient's answer to a write it took, kept to answer a repeat
    /// with. Absent when it refused the write: a refusal changed nothing, so
    /// the claim is released and the write may be tried again.
    #[serde(default)]
    pub response: Option<serde_json::Value>,
}

/// The workspace of a turn this runtime holds.
async fn workspace_of(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
    job_id: Uuid,
) -> Result<Uuid, ApiError> {
    super::router::authorize(state, headers, Authority::WorkTake).await?;
    let job = outturn_core::jobs::get(&state.pool, job_id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "no such turn".to_string()))?;
    let lease = super::work::lease_from(headers)?;
    if job.lease_token != Some(lease) {
        return Err((
            StatusCode::CONFLICT,
            "that turn is not leased to this runtime".to_string(),
        ));
    }
    let payload: super::worker::ChatTurnPayload = serde_json::from_value(job.payload.clone())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("payload: {e}")))?;
    Ok(payload.workspace_id)
}

fn internal(e: sqlx::Error) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn claim(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<Claim>,
) -> Result<Json<Claimed>, ApiError> {
    let workspace = workspace_of(&state, &headers, input.job_id).await?;
    Ok(Json(
        claim_in(&state.pool, workspace, &input.key)
            .await
            .map_err(internal)?,
    ))
}

/// Claims a key, or says what it already holds.
///
/// One statement decides it, so two runtimes sending the same write at once
/// cannot both be told it is new. An expired record is claimed over, as if it
/// had never been.
pub async fn claim_in(
    pool: &sqlx::PgPool,
    workspace: Uuid,
    key: &str,
) -> Result<Claimed, sqlx::Error> {
    // Remembered for a day: see the migration for why.
    let claimed = sqlx::query(
        "insert into tool_invocations (workspace_id, key, state, expires_at) \
         values ($1, $2, 'attempted', now() + interval '24 hours') \
         on conflict (workspace_id, key) do update \
             set state = 'attempted', response = null, created_at = now(), \
                 expires_at = now() + interval '24 hours' \
             where tool_invocations.expires_at < now() \
         returning 1",
    )
    .bind(workspace)
    .bind(key)
    .fetch_optional(pool)
    .await?;
    if claimed.is_some() {
        return Ok(Claimed::New);
    }
    let row = sqlx::query(
        "select state, response, created_at from tool_invocations \
         where workspace_id = $1 and key = $2",
    )
    .bind(workspace)
    .bind(key)
    .fetch_one(pool)
    .await?;
    let at = row.get("created_at");
    Ok(match row.get::<&str, _>("state") {
        "completed" => Claimed::Completed {
            response: row.get("response"),
            at,
        },
        _ => Claimed::Attempted { at },
    })
}

pub async fn settle(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<Settle>,
) -> Result<StatusCode, ApiError> {
    let workspace = workspace_of(&state, &headers, input.job_id).await?;
    settle_in(&state.pool, workspace, &input.key, input.response)
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Records what a claimed write came to: its answer, or a refusal that
/// releases it.
pub async fn settle_in(
    pool: &sqlx::PgPool,
    workspace: Uuid,
    key: &str,
    response: Option<serde_json::Value>,
) -> Result<(), sqlx::Error> {
    match response {
        Some(response) => {
            sqlx::query(
                "update tool_invocations set state = 'completed', response = $3 \
                 where workspace_id = $1 and key = $2",
            )
            .bind(workspace)
            .bind(key)
            .bind(response)
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query(
                "delete from tool_invocations \
                 where workspace_id = $1 and key = $2 and state = 'attempted'",
            )
            .bind(workspace)
            .bind(key)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}
