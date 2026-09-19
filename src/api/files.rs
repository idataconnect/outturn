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

/// The space a session's files live in, checked to be the caller's workspace's
/// and one their authorities reach.
///
/// The scope check belongs here rather than in each handler because this is the
/// one place that knows which agent a session belongs to -- a path names a
/// session, and the agent behind it is what a narrowing is about.
///
/// A person's own session is their own files, the same way it is their own
/// transcript: using an agent and keeping what you produced with it is one
/// permission, and reading what everybody else produced is another.
///
/// The `mine` flag this used to return alongside the space is gone: every
/// caller ignored it once the narrowing check that read it was removed as
/// dead, and a returned value nobody consumes is a question about what it was
/// for.
async fn space_for(
    state: &ApiState,
    claims: &crate::auth::SessionClaims,
    session_id: Uuid,
    authority: Authority,
) -> Result<Space, ApiError> {
    let session = state.chat.get_session(claims.workspace_id, session_id).await?;
    if session.user_id != Some(claims.subject) {
        super::router::require_for_agent(state, claims, authority, session.agent_id).await?;
    }
    Ok(Space {
        workspace_id: claims.workspace_id,
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
    let space = space_for(&state, &claims, session_id, Authority::SessionsRead).await?;
    let store = storage(&state)?;
    let granted = authorities_of(&state, &claims).await?;
    // No narrowing check here. `space_for` was given `SessionsRead`, which is
    // narrowed, so a caller reaching this line on somebody else's session has
    // already been proved to cover its agent -- and every scope below hangs
    // off that same agent. The check that used to stand here re-asked the
    // question and cost a round trip to do it, and could never answer
    // differently. Move it back if `space_for` is ever given an authority that
    // `scope::is_narrowed` does not name.
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

        // A document stored before extraction was configured has no text and
        // nothing scheduled to give it any, so a reader is told to come back
        // shortly for work that will never run. Noticed here because this is
        // what walks every object anyway, and asking is cheap beside the
        // listing that just happened.
        let keys: Vec<String> = found
            .iter()
            .filter(|f| !f.is_dir)
            .map(|f| f.path.clone())
            .collect();
        super::extract::backfill(&state.pool, store.as_ref(), claims.workspace_id, keys).await;

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
    let space = space_for(&state, &claims, session_id, write_authority(s)).await?;
    let store = storage(&state)?;

    let scoped = format!("{}/{path}", s.as_str());
    let key = scope::resolve(&space, &scoped).map_err(storage_failed)?;
    store.write(&key, 0, &body).await.map_err(storage_failed)?;
    // Whatever was read out of the previous version is wrong now.
    super::extract::invalidate(store.as_ref(), &key).await;

    // A document is bytes until something reads it. Queued rather than done
    // here: a scanned file can take minutes, and the upload has already
    // succeeded -- the words arriving later is a file not ready yet, where a
    // request held open for them is a failure the uploader cannot act on.
    super::extract::enqueue(&state.pool, claims.workspace_id, &key, &scoped).await;

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
    let space = space_for(&state, &claims, session_id, read_authority(s)).await?;
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

/// The most a preview reads.
///
/// A preview is a look, not a download: enough to see what a file is and read
/// the top of it. The endpoint beside this one hands over the whole thing, and
/// a text file of forty megabytes rendered into a browser tab helps nobody.
pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;

/// What a preview may be served as, decided by the bytes rather than the name.
///
/// An allowlist and a short one. These files are uploaded by people and
/// written by agents, so serving one inline means running somebody else's
/// content on this origin -- and the session cookie that authorises every API
/// call is on this origin. HTML and SVG are the obvious ways that goes wrong
/// and are deliberately absent: an SVG is a document that can carry script,
/// whatever its extension says.
///
/// The extension is never consulted. `notes.txt` holding a PNG is a PNG, and
/// `photo.png` holding HTML is refused rather than believed.
fn previewable(bytes: &[u8]) -> Option<&'static str> {
    if let Some(image) = crate::runtime::vision::media_type(bytes) {
        return Some(image);
    }
    // Text, by the same reasoning `describe_binary` uses: something with a NUL
    // or a stretch of control characters is not prose, whatever it is called.
    if crate::runtime::component::describe_binary(bytes).is_some() {
        return None;
    }
    // Valid UTF-8 or nothing. A preview that renders replacement characters
    // is a preview of a file somebody should be downloading instead.
    std::str::from_utf8(bytes).ok()?;
    Some("text/plain; charset=utf-8")
}

/// Serves a file for looking at rather than for keeping.
///
/// Separate from `download`, which sends everything as an attachment and
/// should keep doing so: that is what makes it safe to hand over a file
/// nobody has vetted. This one serves inline, so it is narrow on purpose --
/// a short allowlist, sniffed, with the headers that stop a browser having
/// its own opinion.
pub async fn preview(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, scope_name, path)): Path<(Uuid, String, String)>,
) -> Result<Response, ApiError> {
    let s = parse_scope(&scope_name)?;
    let claims = authorize(&state, &headers, read_authority(s)).await?;
    let space = space_for(&state, &claims, session_id, read_authority(s)).await?;
    let store = storage(&state)?;

    let key = scope::resolve(&space, &format!("{}/{path}", s.as_str())).map_err(storage_failed)?;
    // One byte past the bound, so a file exactly at the limit is not reported
    // as truncated and one over it is.
    let bytes = store
        .read(&key, 0, MAX_PREVIEW_BYTES as u32 + 1)
        .await
        .map_err(storage_failed)?;

    let truncated = bytes.len() > MAX_PREVIEW_BYTES;
    let shown = &bytes[..bytes.len().min(MAX_PREVIEW_BYTES)];

    let Some(media_type) = previewable(shown) else {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "this file has no preview".to_string(),
        ));
    };

    Ok((
        [
            (header::CONTENT_TYPE, media_type.to_string()),
            // Inline, which is the whole point, but under a sandbox: even if
            // something got through the allowlist it runs as its own origin
            // with no script, so it cannot reach the cookie that authorises
            // this API.
            (header::CONTENT_DISPOSITION, "inline".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'; style-src 'unsafe-inline'".to_string(),
            ),
            // No guessing. Without this a browser may decide a text file is
            // HTML because it starts with a tag, which is exactly the path the
            // allowlist is trying to close.
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            // Said in a header rather than in the body, so the bytes are the
            // file and nothing else.
            (
                header::HeaderName::from_static("x-outturn-truncated"),
                truncated.to_string(),
            ),
        ],
        shown.to_vec(),
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
    let space = space_for(&state, &claims, session_id, write_authority(s)).await?;
    let store = storage(&state)?;

    let key = scope::resolve(&space, &format!("{}/{path}", s.as_str())).map_err(storage_failed)?;
    store.delete(&key).await.map_err(storage_failed)?;
    // And its text, or a deleted report stays readable to anyone who names it.
    super::extract::invalidate(store.as_ref(), &key).await;
    tracing::info!(actor = %claims.subject, session_id = %session_id, path = %path, "file deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_what_its_bytes_say_rather_than_what_it_is_called() {
        // The name comes from whoever uploaded it or whatever the agent wrote,
        // so it is a claim rather than a fact. Every decision here is made on
        // the bytes.
        assert_eq!(previewable(b"just some prose"), Some("text/plain; charset=utf-8"));
        assert_eq!(previewable(b"\x89PNG\r\n\x1a\n and so on"), Some("image/png"));
        assert_eq!(previewable(b"\xff\xd8\xff\xe0 jpeg"), Some("image/jpeg"));
    }

    #[test]
    fn nothing_that_could_run_is_served_inline() {
        // The reason the allowlist is short. These files are written by agents
        // and uploaded by people, and this API's session cookie lives on the
        // origin that would run them. HTML is refused as HTML, and refused
        // again for being served under a name that suggests otherwise.
        let html = b"<html><script>alert(1)</script></html>";
        assert_eq!(
            previewable(html),
            Some("text/plain; charset=utf-8"),
            "html is shown as text, never as a document"
        );

        // An SVG is a document that can carry script. It is text, so it comes
        // back as text -- which is the point: it is never `image/svg+xml`, and
        // a browser told `text/plain` with `nosniff` will not render it.
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";
        assert_eq!(previewable(svg), Some("text/plain; charset=utf-8"));
        assert_ne!(previewable(svg), Some("image/svg+xml"));
    }

    #[test]
    fn something_nobody_can_read_has_no_preview() {
        // A zip, a PDF, anything with a NUL in it. The modal says so and
        // offers the download instead, which is a better answer than a page
        // of replacement characters.
        assert_eq!(previewable(b"PK\x03\x04\x00\x00"), None);
        assert_eq!(previewable(b"%PDF-1.7\n\x00\x01\x02"), None);
        assert_eq!(previewable(&[0x00, 0x01, 0x02, 0x03]), None);
    }

    #[test]
    fn broken_text_is_not_offered_as_text() {
        // Invalid UTF-8 renders as replacement characters, which looks like a
        // fault in the file rather than in the preview.
        assert_eq!(previewable(&[0xff, 0xfe, b'h', b'i']), None);
    }

    #[test]
    fn an_empty_file_previews_as_empty_rather_than_refusing() {
        // Nothing is wrong with it, and "no preview" would read as a fault.
        assert_eq!(previewable(b""), Some("text/plain; charset=utf-8"));
    }
}
