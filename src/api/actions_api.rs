//! The notification centre: what is waiting on the person reading.
//!
//! Global rather than per-workspace, so a decision owed in a workspace they are
//! not looking at is still seen. Which is why these handlers authenticate and
//! then stop: there is no workspace to resolve an authority against, because a
//! token carries one workspace and the answer spans all of them.
//!
//! That is not a gap. Both reads are confined to items addressed to the caller
//! -- by name, or through a role they currently hold -- so there is nothing
//! here to escalate to. An authority check could only ever refuse somebody
//! their own work, and gating on the token's workspace would be worse than
//! nothing: it would let a role grant in one workspace license reading
//! another's. Membership is the tenancy check, and it is in the query.
//!
//! Settling is the opposite case and is not here. Answering an approval acts
//! on one workspace's item, so it belongs on a route that authorises against
//! that workspace.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use super::actions::ActionItem;
use super::router::{ApiError, ApiState};

/// Items returned in one page of the queue.
///
/// A screen's worth. The queue is what a person is expected to work through,
/// so a reader with more than this owed is not served by all of it arriving at
/// once.
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

/// Where the badge stops counting.
///
/// Rendered as "and more" past this, so counting further buys nothing and costs
/// a scan bounded by somebody else's backlog rather than by what is readable.
const BADGE_CAP: i64 = 99;

/// How long a waiting request parks before returning what it has.
///
/// Matched to the events poll, for the same reasons: inside proxy idle
/// timeouts, and long enough that an idle client is not constantly
/// reconnecting.
const POLL_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Deserialize)]
pub struct QueueQuery {
    pub limit: Option<i64>,
    pub after: Option<uuid::Uuid>,
    /// Park until the queue changes rather than answering at once.
    ///
    /// The queue is a set rather than a log, so there is no cursor to compare
    /// against and "changed" cannot be decided from the request alone. What a
    /// waiter is told is that *something* concerning them landed; it then
    /// re-reads and sends the whole page. A client that wants the current
    /// answer immediately omits this.
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Serialize)]
pub struct BadgeResponse {
    /// Open items waiting on the caller, across every workspace they belong to.
    pub count: i64,
    /// Whether the count stopped at the cap. The client renders "99+" rather
    /// than a total when this is set.
    pub capped: bool,
}

/// What is waiting on the caller, oldest first.
pub async fn queue(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<QueueQuery>,
) -> Result<Json<super::Page<ActionItem>>, ApiError> {
    // Authenticated, not authorised: see the module comment.
    let claims = super::router::authenticate(&state, &headers)?;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    if query.wait {
        return wait_for_change(&state, claims.subject, query.after, limit)
            .await
            .map(Json);
    }

    let items = state
        .actions
        .queue_for_user_everywhere(claims.subject, query.after, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(super::Page::from_rows(items, |i| i.id)))
}

/// How many items are waiting on the caller. Read on every page load.
pub async fn badge(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<BadgeResponse>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;

    // Asked for one past the cap, so "exactly at the cap" and "more than the
    // cap" can be told apart -- otherwise a count of 99 would render as "99+"
    // and overstate by one at the only point anybody notices.
    let count = state
        .actions
        .count_for_user_everywhere(claims.subject, BADGE_CAP + 1)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let capped = count > BADGE_CAP;
    Ok(Json(BadgeResponse {
        count: count.min(BADGE_CAP),
        capped,
    }))
}

/// Parks until something concerning this person lands, then reads the queue.
///
/// Subscribing happens before the first read, so a change arriving between the
/// two is caught by the wait rather than missed. Returns the queue as it stands
/// on timeout as well, so a client that hears nothing still has an answer.
async fn wait_for_change(
    state: &ApiState,
    user_id: uuid::Uuid,
    after: Option<uuid::Uuid>,
    limit: i64,
) -> Result<super::Page<ActionItem>, ApiError> {
    use tokio::sync::broadcast::error::RecvError;

    let mut rx = state.action_bus.subscribe();

    // Read once when parking rather than per hint: one announcement fanned out
    // to every waiter on the pod must not become a query each.
    let roles = state
        .actions
        .roles_of(user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let read = || async {
        let items = state
            .actions
            .queue_for_user_everywhere(user_id, after, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        Ok(super::Page::from_rows(items, |i| i.id))
    };

    let deadline = tokio::time::sleep(POLL_TIMEOUT);
    tokio::pin!(deadline);
    let shutdown = Arc::clone(&state.shutdown).notified_owned();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut deadline => return read().await,
            _ = &mut shutdown => return read().await,
            received = rx.recv() => match received {
                Ok(hint) => {
                    if hint.concerns(user_id, &roles) {
                        return read().await;
                    }
                }
                Err(RecvError::Lagged(_)) => return read().await,
                Err(RecvError::Closed) => return read().await,
            },
        }
    }
}
