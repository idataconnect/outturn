//! Where a conversation's model changed: the replies a model other than the
//! one its chain tried first answered, where the reply before was not answered
//! by that model too, and why: what the gateway recorded as it fell over
//! (`reply_fallbacks`). Read beside the transcript, so staff can see when a
//! fallback answered and the voice may differ. See docs/models.md.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use outturn_core::auth::rbac::Authority;

use super::router::{ApiError, ApiState, authorize};
use super::sessions::{Ownership, session_for};

#[derive(Debug, Serialize)]
pub struct ModelChange {
    pub reply_id: Uuid,
    /// The model the chain tried first, by its id and, where it is still
    /// configured, the name the operator gave it.
    pub configured_model: String,
    pub configured_name: Option<String>,
    pub served_model: String,
    pub served_name: Option<String>,
    /// Why the first was passed over, as a clause: "was rate limited".
    pub reason: String,
}

pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ModelChange>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    session_for(
        &state,
        &claims,
        id,
        Authority::SessionsRead,
        Ownership::Suffices,
    )
    .await?;
    // Only where the model changes in the conversation: a reply answered by
    // the same fallback as the reply before it says nothing new, and a
    // conversation whose first model stays down would otherwise carry the
    // note on every reply. Compared with what served the reply before, which
    // each reply records.
    let rows = sqlx::query(
        "with replies as ( \
             select id, lag(model) over (order by id) as before \
             from agent_messages where session_id = $1 and role = 'assistant' \
         ) \
         select f.reply_id, f.configured_model, f.configured_name, f.served_model, \
                f.served_name, f.reason \
         from reply_fallbacks f join replies r on r.id = f.reply_id \
         where f.session_id = $1 and r.before is distinct from f.served_model \
         order by f.reply_id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        rows.iter()
            .map(|r| ModelChange {
                reply_id: r.get("reply_id"),
                configured_model: r.get("configured_model"),
                configured_name: r.get("configured_name"),
                served_model: r.get("served_model"),
                served_name: r.get("served_name"),
                reason: r.get("reason"),
            })
            .collect(),
    ))
}
