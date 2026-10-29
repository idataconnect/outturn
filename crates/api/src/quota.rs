//! A workspace's storage quota, as the API reads it: the setting, what the
//! workspace holds, and the refusal when a write would take it past.
//! See `docs/storage-quota.md`.

use axum::http::StatusCode;
use uuid::Uuid;

use outturn_core::storage::stored;

use crate::router::{ApiError, ApiState};
use crate::settings::{Level, SettingsStore};

/// `max_stored_bytes` as the workspace has it; zero is no limit.
pub async fn quota(
    settings: &dyn SettingsStore,
    workspace_id: Uuid,
) -> Result<u64, crate::settings::SettingsError> {
    let view = settings.view(Level::Workspace(workspace_id), None).await?;
    Ok(view
        .iter()
        .find(|e| e.setting.key == "max_stored_bytes")
        .and_then(|e| e.value.as_u64())
        .unwrap_or(0))
}

/// What the workspace may still write, in bytes, or `None` for no limit.
/// What a turn is handed, and what an upload is checked against.
pub async fn remaining(
    settings: &dyn SettingsStore,
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
) -> anyhow::Result<Option<u64>> {
    let quota = quota(settings, workspace_id).await?;
    Ok(stored::remaining(pool, workspace_id, quota).await?)
}

/// Refuses writing `bytes` to `keys` where it would take the workspace past
/// its quota. What the keys hold now is given back first: replacing a file
/// with a smaller one is never refused for being over.
pub async fn check(
    state: &ApiState,
    workspace_id: Uuid,
    keys: &[String],
    bytes: u64,
) -> Result<(), ApiError> {
    let Some(left) = remaining(state.settings.as_ref(), &state.pool, workspace_id)
        .await
        .map_err(internal)?
    else {
        return Ok(());
    };
    let replaced: Option<i64> =
        sqlx::query_scalar("select sum(bytes)::bigint from stored_objects where key = any($1)")
            .bind(keys)
            .fetch_one(&state.pool)
            .await
            .map_err(internal)?;
    let left = left.saturating_add(u64::try_from(replaced.unwrap_or(0)).unwrap_or(0));
    if bytes > left {
        return Err(full());
    }
    Ok(())
}

/// The refusal, the same wherever a write is refused for it.
pub fn full() -> ApiError {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        "this workspace's storage is full: files can still be read and deleted, \
         but not written"
            .into(),
    )
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// What a workspace holds and may hold, as the pages show it.
#[derive(Debug, serde::Serialize)]
pub struct Storage {
    /// What it holds, in bytes, as last counted.
    pub stored_bytes: u64,
    /// Zero for no limit.
    pub quota: u64,
    /// Whether the holdings have been listed from the store yet. Until they
    /// have, no quota applies.
    pub counted: bool,
    /// Whether a write would be refused now.
    pub full: bool,
}

/// What one workspace holds and may hold.
async fn storage_of(state: &ApiState, workspace_id: Uuid) -> Result<Storage, ApiError> {
    let quota = quota(state.settings.as_ref(), workspace_id).await?;
    let (held, counted): (i64, bool) = sqlx::query_as(
        "select stored_bytes, stored_counted_at is not null from workspaces where id = $1",
    )
    .bind(workspace_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or((StatusCode::NOT_FOUND, "no such workspace".to_string()))?;
    let stored_bytes = u64::try_from(held).unwrap_or(0);
    Ok(Storage {
        stored_bytes,
        quota,
        counted,
        full: counted && quota > 0 && stored_bytes >= quota,
    })
}

/// `GET /v1/storage`: for anybody in the workspace, since anybody uploading
/// is who needs to know there is no room.
pub async fn view(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Storage>, ApiError> {
    let claims = crate::router::authenticate(&state, &headers)?;
    Ok(axum::Json(storage_of(&state, claims.workspace_id).await?))
}

/// `GET /v1/workspaces/{id}/storage`: any workspace's, for the operator.
pub async fn view_as_operator(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::Json<Storage>, ApiError> {
    let claims = crate::router::authenticate(&state, &headers)?;
    if !claims.is_system_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator sees another workspace's storage".into(),
        ));
    }
    Ok(axum::Json(storage_of(&state, id).await?))
}
