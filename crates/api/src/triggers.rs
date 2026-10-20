//! An agent's triggers, every kind in one list. See docs/triggers.md, "The API
//! and the page".
//!
//! A row carries what every kind has -- name, prompt, enabled, the ceiling and
//! what last happened -- and the kind's own few fields in `config`, so a reader
//! that knows nothing of a kind can still list, describe and judge it. A
//! webhook's secret is never in it: that is said once, when the hook is made.
//! Creating and editing stay with each kind's own routes, whose forms differ.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use sqlx::Row;
use uuid::Uuid;

use super::router::{ApiError, ApiState, authorize};
use outturn_core::auth::rbac::Authority;

#[derive(Debug, serde::Serialize)]
pub struct Trigger {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub prompt: String,
    pub enabled: bool,
    /// The kind's own: a schedule's expression and zone, a hook's path and
    /// scheme. Never a secret.
    pub config: serde_json::Value,
    pub max_per_hour: Option<i32>,
    pub max_concurrent: Option<i32>,
    pub refused: i32,
    pub last_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub next_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list_for_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(agent_id): Path<Uuid>,
) -> Result<Json<super::Page<Trigger>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let rows = sqlx::query(
        "select id, kind, name, prompt, enabled, config - 'secret' as config, max_per_hour, \
                max_concurrent, refused, last_at, last_status, last_error, next_at \
           from triggers where workspace_id = $1 and agent_id = $2 order by name, id",
    )
    .bind(claims.workspace_id)
    .bind(agent_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let items = rows
        .iter()
        .map(|r| Trigger {
            id: r.get("id"),
            kind: r.get("kind"),
            name: r.get("name"),
            prompt: r.get("prompt"),
            enabled: r.get("enabled"),
            config: r.get("config"),
            max_per_hour: r.get("max_per_hour"),
            max_concurrent: r.get("max_concurrent"),
            refused: r.get("refused"),
            last_at: r.get("last_at"),
            last_status: r.get("last_status"),
            last_error: r.get("last_error"),
            next_at: r.get("next_at"),
        })
        .collect();
    // An agent has a handful; one page holds them all.
    Ok(Json(super::Page { items, next: None }))
}
