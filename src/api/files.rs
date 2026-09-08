//! Files a person puts into, or takes out of, a conversation's storage.
//!
//! The same three scopes the agent sees -- session/, agent/, workspace/ -- with
//! the same layout underneath, so a file a person uploads is a file the agent
//! can list and read by the same name, and a file the agent wrote is one the
//! person can download. The browser uploads through this tier rather than the
//! runtime because the runtime holds no credentials for anything but the turn
//! it is running, and never should.
//!
//! Who may touch which scope is an authority per scope (docs/authorities.md).
//! Session scope needs only what being in the conversation already needs; the
//! longer-lived scopes need the storage authorities.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::Authority;
use crate::runtime::storage::scope::{self, Scope, Space};

use super::router::{ApiError, ApiState, authorities_of, authorize};

/// The most a single upload may be. Larger files are what the ranged read
/// exists for on the agent's side, but they still have to get in, and a
/// browser upload of hundreds of megabytes through an API pod is not the
/// path for that.
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct StoredFile {
    /// As the agent would name it: `session/report.pdf`.
    pub path: String,
    pub scope: &'static str,
    pub size: u64,
}

fn read_authority(scope: Scope) -> Authority {
    match scope {
        Scope::Session => Authority::SessionsRead,
        Scope::Agent => Authority::StorageAgentRead,
        Scope::Workspace => Authority::StorageWorkspaceRead,
    }
}

fn write_authority(scope: Scope) -> Authority {
    match scope {
        Scope::Session => Authority::SessionsCreate,
        Scope::Agent => Authority::StorageAgentWrite,
        Scope::Workspace => Authority::StorageWorkspaceWrite,
    }
}

fn parse_scope(s: &str) -> Result<Scope, ApiError> {
    Scope::parse(s).ok_or((
        StatusCode::NOT_FOUND,
        format!("{s} is not a scope; use session, agent or workspace"),
    ))
}

/// The space a session's files live in, checked to be the caller's workspace's.
async fn space_for(state: &ApiState, workspace_id: Uuid, session_id: Uuid) -> Result<Space, ApiError> {
    let session = state.chat.get_session(workspace_id, session_id).await?;
    Ok(Space {
        workspace_id,
        agent_id: session.agent_id,
        session_id,
    })
}

fn storage(state: &ApiState) -> Result<Arc<dyn crate::runtime::storage::StorageBackend>, ApiError> {
    state.storage.clone().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "object storage is not configured".into(),
    ))
}

fn storage_failed(e: crate::runtime::storage::StorageError) -> ApiError {
    use crate::runtime::storage::StorageError;
    match e {
        StorageError::NotFound => (StatusCode::NOT_FOUND, "no such file".into()),
        StorageError::PermissionDenied => (StatusCode::BAD_REQUEST, "that path is not allowed".into()),
        StorageError::Refused(m) => (StatusCode::BAD_REQUEST, m),
        StorageError::Unavailable(m) => {
            tracing::warn!(error = %m, "object storage failed serving a file request");
            (StatusCode::SERVICE_UNAVAILABLE, "object storage is unavailable".into())
        }
        StorageError::Io(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

/// Every file the caller may see across the three scopes of this session.
pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Vec<StoredFile>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    let space = space_for(&state, claims.workspace_id, session_id).await?;
    let store = storage(&state)?;
    let granted = authorities_of(&state, &claims).await?;

    let mut out = Vec::new();
    for s in Scope::ALL {
        // Scopes the caller may not read are left out rather than refused, so
        // a viewer sees their conversation's files and nothing about what
        // else exists.
        if !granted.contains(&read_authority(s)) {
            continue;
        }
        let found = store
            .list(&scope::root_for(&space, s))
            .await
            .map_err(storage_failed)?;
        out.extend(found.iter().filter(|f| !f.is_dir).filter_map(|f| {
            Some(StoredFile {
                path: scope::strip_root(&space, &f.path)?,
                scope: s.as_str(),
                size: f.size,
            })
        }));
    }
    Ok(Json(out))
}

/// Writes a file, replacing anything at that path.
pub async fn upload(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, scope_name, path)): Path<(Uuid, String, String)>,
    body: Bytes,
) -> Result<(StatusCode, Json<StoredFile>), ApiError> {
    let s = parse_scope(&scope_name)?;
    let claims = authorize(&state, &headers, write_authority(s)).await?;
    let space = space_for(&state, claims.workspace_id, session_id).await?;
    let store = storage(&state)?;

    let scoped = format!("{}/{path}", s.as_str());
    let key = scope::resolve(&space, &scoped).map_err(storage_failed)?;
    store.write(&key, 0, &body).await.map_err(storage_failed)?;

    tracing::info!(
        actor = %claims.subject,
        session_id = %session_id,
        path = %scoped,
        bytes = body.len(),
        "file uploaded"
    );
    Ok((
        StatusCode::CREATED,
        Json(StoredFile {
            path: scoped,
            scope: s.as_str(),
            size: body.len() as u64,
        }),
    ))
}

/// The file, as bytes. The browser decides what to do with it from the name.
pub async fn download(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, scope_name, path)): Path<(Uuid, String, String)>,
) -> Result<Response, ApiError> {
    let s = parse_scope(&scope_name)?;
    let claims = authorize(&state, &headers, read_authority(s)).await?;
    let space = space_for(&state, claims.workspace_id, session_id).await?;
    let store = storage(&state)?;

    let key = scope::resolve(&space, &format!("{}/{path}", s.as_str())).map_err(storage_failed)?;
    let bytes = store.read(&key, 0, u32::MAX).await.map_err(storage_failed)?;
    let name = path.rsplit('/').next().unwrap_or("file").replace('"', "");

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

pub async fn delete(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, scope_name, path)): Path<(Uuid, String, String)>,
) -> Result<StatusCode, ApiError> {
    let s = parse_scope(&scope_name)?;
    let claims = authorize(&state, &headers, write_authority(s)).await?;
    let space = space_for(&state, claims.workspace_id, session_id).await?;
    let store = storage(&state)?;

    let key = scope::resolve(&space, &format!("{}/{path}", s.as_str())).map_err(storage_failed)?;
    store.delete(&key).await.map_err(storage_failed)?;
    tracing::info!(actor = %claims.subject, session_id = %session_id, path = %path, "file deleted");
    Ok(StatusCode::NO_CONTENT)
}
