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

    // Events are read against the token's tenant, never a caller-supplied one,
    // so a cursor cannot be used to reach across tenants.
    let found = events::wait_for(
        &state.pool,
        &state.bus,
        claims.tenant_id,
        query.session_id,
        query.after,
        limit,
        POLL_TIMEOUT,
        Arc::clone(&state.shutdown).notified_owned(),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let cursor = found.last().map(|e| e.id).unwrap_or(query.after);

    Ok(Json(PollResponse {
        events: found,
        cursor,
    }))
}
