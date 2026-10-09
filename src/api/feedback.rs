//! A person's verdict on a reply, or on a whole conversation.
//!
//! Rating needs what reading needs and nothing more: somebody who may read a
//! conversation may say what they thought of it, and somebody who may not
//! cannot learn, by rating, that it exists. Each person has one verdict per
//! target, which they change or withdraw; everybody who may read the
//! conversation sees all of them, named by the rule in `actor`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use super::actor::Actor;
use super::router::{ApiError, ApiState, authorize};
use super::sessions::{Ownership, session_for};
use crate::auth::rbac::Authority;

/// Long enough to say what went wrong, short enough not to be a document.
const MAX_NOTE_CHARS: usize = 2_000;

#[derive(Debug, Serialize)]
pub struct Feedback {
    pub id: Uuid,
    /// The reply it is about, or none for the conversation as a whole.
    pub message_id: Option<Uuid>,
    pub verdict: String,
    pub note: String,
    pub by: Actor,
    /// Whether it is the reader's own, which is what they may change.
    pub mine: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct Give {
    #[serde(default)]
    pub message_id: Option<Uuid>,
    pub verdict: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct Target {
    #[serde(default)]
    pub message_id: Option<Uuid>,
}

fn internal(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "feedback query failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error".to_string(),
    )
}

/// Every verdict on a conversation, its replies' and its own.
pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Feedback>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    session_for(
        &state,
        &claims,
        id,
        Authority::SessionsRead,
        Ownership::Suffices,
    )
    .await?;

    let rows = sqlx::query(
        "select id, message_id, user_id, verdict, note, updated_at \
         from feedback where session_id = $1 order by created_at",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    let ids: Vec<Uuid> = rows.iter().map(|r| r.get("user_id")).collect();
    let names = super::actor::users(&state.pool, &ids)
        .await
        .map_err(internal)?;

    Ok(Json(
        rows.iter()
            .map(|r| {
                let user: Uuid = r.get("user_id");
                Feedback {
                    id: r.get("id"),
                    message_id: r.get("message_id"),
                    verdict: r.get("verdict"),
                    note: r.get("note"),
                    by: names.get(&user).cloned().unwrap_or_default(),
                    mine: user == claims.subject,
                    updated_at: r.get("updated_at"),
                }
            })
            .collect(),
    ))
}

/// Gives the reader's verdict, or changes the one they gave.
pub async fn give(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<Give>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    session_for(
        &state,
        &claims,
        id,
        Authority::SessionsRead,
        Ownership::Suffices,
    )
    .await?;

    if !matches!(input.verdict.as_str(), "up" | "down") {
        return Err((StatusCode::BAD_REQUEST, "a verdict is up or down".into()));
    }
    let note = input.note.trim();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err((StatusCode::BAD_REQUEST, "the note is too long".into()));
    }

    // A reply of this conversation, and one the agent wrote: a verdict on a
    // person's own message is a verdict on the person, which is not what this
    // is for, and one naming another conversation's reply would attach to
    // whatever the id happened to be.
    if let Some(message) = input.message_id {
        let ok: bool = sqlx::query_scalar(
            "select exists (select 1 from agent_messages \
             where id = $1 and session_id = $2 and role = 'assistant')",
        )
        .bind(message)
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .map_err(internal)?;
        if !ok {
            return Err((StatusCode::NOT_FOUND, "no such reply".into()));
        }
    }

    sqlx::query(
        "insert into feedback (id, workspace_id, session_id, message_id, user_id, verdict, note) \
         values ($1, $2, $3, $4, $5, $6, $7) \
         on conflict (session_id, coalesce(message_id, '00000000-0000-0000-0000-000000000000'), user_id) \
         do update set verdict = excluded.verdict, note = excluded.note, updated_at = now()",
    )
    .bind(Uuid::now_v7())
    .bind(claims.workspace_id)
    .bind(id)
    .bind(input.message_id)
    .bind(claims.subject)
    .bind(&input.verdict)
    .bind(note)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Withdraws the reader's verdict. Nobody else's: each person speaks for
/// themselves.
pub async fn withdraw(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Query(target): Query<Target>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    session_for(
        &state,
        &claims,
        id,
        Authority::SessionsRead,
        Ownership::Suffices,
    )
    .await?;

    sqlx::query(
        "delete from feedback where session_id = $1 and user_id = $2 \
           and message_id is not distinct from $3",
    )
    .bind(id)
    .bind(claims.subject)
    .bind(target.message_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    Ok(StatusCode::NO_CONTENT)
}
