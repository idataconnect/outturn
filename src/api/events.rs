use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Authority;
use crate::events;

use super::router::{ApiError, ApiState};

/// Long polls park for this long before returning empty. Short enough to stay
/// well inside proxy idle timeouts, long enough that reconnect churn is low.
const POLL_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_LIMIT: i64 = 500;

#[derive(Debug, Deserialize)]
pub struct PollQuery {
    /// Cursor: return events whose id sorts above this. Absent means "from the
    /// beginning", since a nil UUID sorts below every UUIDv7.
    #[serde(default)]
    pub after: Uuid,
    pub session_id: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PollResponse {
    pub events: Vec<events::Event>,
    /// Cursor to pass as `after` on the next request. Unchanged when the poll
    /// timed out with nothing new.
    pub cursor: Uuid,
}

pub async fn poll(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<PollQuery>,
) -> Result<Json<PollResponse>, ApiError> {
    let claims = super::router::authorize(&state, &headers, Authority::SessionsRead).await?;

    let limit = query.limit.unwrap_or(100).clamp(1, MAX_LIMIT);

    // The same narrowing the session list and the message read already apply.
    // Without it a person scoped to one agent is refused the transcript and
    // then served the same words here as they are streamed -- the feed is the
    // conversation, arriving a little earlier.
    let reach = super::router::reach_of(&state, &claims).await?;
    let visible = events::Visible::of(&reach, claims.subject);

    // Events are read against the token's workspace, never a caller-supplied one,
    // so a cursor cannot be used to reach across workspaces.
    let found = events::wait_for(
        &state.pool,
        &state.bus,
        claims.workspace_id,
        query.session_id,
        query.after,
        limit,
        POLL_TIMEOUT,
        Arc::clone(&state.shutdown).notified_owned(),
        visible.as_ref(),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Over what was filtered out as well as what came back. A narrowed caller
    // whose window held nothing for them would otherwise be handed their own
    // cursor back and re-poll from it forever, rescanning a span that only
    // grows -- so the cursor moves to the end of the window that was examined,
    // which is bounded by the same limit the read used and therefore skips
    // nothing that has yet to arrive.
    let cursor = match found.last() {
        Some(last) => last.id,
        None if visible.is_some() => events::watermark(
            &state.pool,
            claims.workspace_id,
            query.session_id,
            query.after,
            limit,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .unwrap_or(query.after),
        None => query.after,
    };

    Ok(Json(PollResponse {
        events: found,
        cursor,
    }))
}
