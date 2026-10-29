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

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use uuid::Uuid;

use outturn_core::auth::Authority;
use outturn_core::storage::scope::{self, Scope, Space};

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
    /// Where the original is, when this is a JPEG made from a HEIC photo at
    /// upload. Absent for everything else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub converted_from: Option<String>,
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
    claims: &outturn_core::auth::SessionClaims,
    session_id: Uuid,
    authority: Authority,
) -> Result<Space, ApiError> {
    let session = state
        .chat
        .get_session(claims.workspace_id, session_id)
        .await?;
    if session.user_id != Some(claims.subject) {
        super::router::require_for_agent(state, claims, authority, session.agent_id).await?;
    }
    Ok(Space {
        workspace_id: claims.workspace_id,
        agent_id: session.agent_id,
        session_id,
    })
}

pub(super) fn storage(
    state: &ApiState,
) -> Result<Arc<dyn outturn_core::storage::StorageBackend>, ApiError> {
    state.storage.clone().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "object storage is not configured".into(),
    ))
}

pub(super) fn storage_failed(e: outturn_core::storage::StorageError) -> ApiError {
    use outturn_core::storage::StorageError;
    match e {
        StorageError::NotFound => (StatusCode::NOT_FOUND, "no such file".into()),
        StorageError::PermissionDenied => {
            (StatusCode::BAD_REQUEST, "that path is not allowed".into())
        }
        StorageError::Refused(m) => (StatusCode::BAD_REQUEST, m),
        StorageError::Unavailable(m) => {
            tracing::warn!(error = %m, "object storage failed serving a file request");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "object storage is unavailable".into(),
            )
        }
        StorageError::Io(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

/// What `list` takes: a scope and a cursor to read one scope's next page, or
/// neither for the first page of every scope the caller may read.
#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    pub scope: Option<String>,
    /// The last path a previous page ended on, within the scope: `report.pdf`
    /// for `workspace/report.pdf`.
    pub after: Option<String>,
    pub limit: Option<usize>,
}

/// A page of a conversation's files, and where each scope that has more goes on.
#[derive(Debug, Serialize)]
pub struct Listed {
    pub items: Vec<StoredFile>,
    /// For each scope with more files than this page held, the cursor to pass
    /// back as `after` with that `scope`. A scope with nothing more is absent.
    pub more: std::collections::BTreeMap<&'static str, String>,
}

/// The files the caller may see in this session's scopes, a page at a time.
///
/// Paged per scope rather than across all three, because they are three
/// separate listings in the store and a single cursor across them would have
/// to say where it was in each. The workspace scope is the one that grows --
/// every conversation in the workspace adds to it -- and it was read whole on
/// every open of the Files tab.
pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(session_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<Json<Listed>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    let space = space_for(&state, &claims, session_id, Authority::SessionsRead).await?;
    let store = storage(&state)?;
    let granted = authorities_of(&state, &claims).await?;
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    // One scope when continuing, all of them on a first read. A cursor
    // without a scope is a cursor into nothing in particular.
    let scopes: Vec<Scope> = match query.scope.as_deref() {
        Some(name) => vec![Scope::parse(name).ok_or((
            StatusCode::BAD_REQUEST,
            format!("no scope {name}; it is session, agent or workspace"),
        ))?],
        None if query.after.is_some() => {
            return Err((
                StatusCode::BAD_REQUEST,
                "`after` continues one scope, so it needs `scope` too".to_string(),
            ));
        }
        None => Scope::ALL.to_vec(),
    };
    // No narrowing check here. `space_for` was given `SessionsRead`, which is
    // narrowed, so a caller reaching this line on somebody else's session has
    // already been proved to cover its agent -- and every scope below hangs
    // off that same agent. The check that used to stand here re-asked the
    // question and cost a round trip to do it, and could never answer
    // differently. Move it back if `space_for` is ever given an authority that
    // `scope::is_narrowed` does not name.
    let mut out = Listed {
        items: Vec::new(),
        more: Default::default(),
    };
    for s in scopes {
        // Scopes the caller may not read are left out rather than refused, so
        // a viewer sees their conversation's files and nothing about what
        // else exists.
        if !granted.contains(&read_authority(s)) {
            continue;
        }
        let root = scope::root_for(&space, s);
        // The cursor is joined under this scope's own root, so whatever it
        // says, the page cannot start anywhere but inside it.
        let after = query.after.as_deref().map(|a| format!("{root}{a}"));
        let (found, more) = store
            .list_page(&root, after.as_deref(), limit)
            .await
            .map_err(storage_failed)?;

        // A document stored before extraction was configured has no text and
        // nothing scheduled to give it any, so a reader is told to come back
        // shortly for work that will never run. Noticed here because this is
        // what walks the objects anyway -- and done after answering, since
        // checking costs storage round trips per file the reader is not
        // waiting on.
        let keys: Vec<String> = found
            .iter()
            .filter(|f| !f.is_dir)
            .map(|f| f.path.clone())
            .collect();
        super::extract::backfill_later(
            state.pool.clone(),
            store.clone(),
            claims.workspace_id,
            root.clone(),
            keys,
        );

        if more && let Some(last) = found.last().and_then(|f| f.path.strip_prefix(&root)) {
            out.more.insert(s.as_str(), last.to_string());
        }
        out.items
            .extend(found.iter().filter(|f| !f.is_dir).filter_map(|f| {
                Some(StoredFile {
                    path: scope::strip_root(&space, &f.path)?,
                    scope: s.as_str(),
                    size: f.size,
                    converted_from: None,
                })
            }));
    }
    Ok(Json(out))
}

/// Writes a file, replacing anything at that path.
///
/// A HEIC photo is also stored as a JPEG beside it, and what is returned is
/// the JPEG: that is the file to mention to an agent.
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

    let mut scoped = format!("{}/{path}", s.as_str());

    // A phone's photo becomes a JPEG before anything is stored, so a photo
    // that cannot be converted is a failed upload the person sees now, not a
    // file an agent fails to read later. The original is kept beside it.
    let mut original: Option<(String, Bytes)> = None;
    let body = if super::heic::is_heif(&body) {
        let Some(url) = &state.heic_url else {
            return Err((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "HEIC photos are not converted on this deployment. \
                 A JPEG or PNG of the photo can be uploaded instead."
                    .into(),
            ));
        };
        let jpeg = super::heic::to_jpeg(url, body.clone()).await?;

        // Both named from what they are rather than what they were called.
        // The original replaces whatever was at its own name, as any upload
        // does; anything renamed takes a name nobody is using, so a JPEG made
        // here never lands on a file somebody put there.
        let kept = super::heic::original_path(&scoped);
        let kept = if kept == scoped {
            kept
        } else {
            free_name(
                store.as_ref(),
                &space,
                super::heic::candidates(&scoped, "heic"),
            )
            .await?
        };
        let converted = free_name(
            store.as_ref(),
            &space,
            super::heic::candidates(&scoped, "jpg").filter(|p| *p != kept),
        )
        .await?;
        original = Some((kept, body));
        scoped = converted;
        Bytes::from(jpeg)
    } else {
        body
    };

    let key = scope::resolve(&space, &scoped).map_err(storage_failed)?;
    let kept_key = match &original {
        Some((kept, _)) => Some(scope::resolve(&space, kept).map_err(storage_failed)?),
        None => None,
    };
    // Before anything is stored, against what the workspace holds less what
    // these replace.
    let keys: Vec<String> = std::iter::once(key.clone())
        .chain(kept_key.clone())
        .collect();
    let bytes = body.len() + original.as_ref().map_or(0, |(_, b)| b.len());
    super::quota::check(&state, claims.workspace_id, &keys, bytes as u64).await?;

    store.write(&key, &body).await.map_err(storage_failed)?;
    // Whatever was read out of the previous version is wrong now.
    super::extract::invalidate(store.as_ref(), &key).await;
    counted(&state, &key, body.len()).await;

    // The JPEG first and the original after it, so that a failure between
    // the two can be undone by removing one file: an upload either stores
    // both or neither.
    if let Some((kept, bytes)) = &original {
        let written = match scope::resolve(&space, kept) {
            Ok(at) => store.write(&at, bytes).await.map(|_| at),
            Err(e) => Err(e),
        };
        match written {
            Ok(at) => {
                super::extract::invalidate(store.as_ref(), &at).await;
                counted(&state, &at, bytes.len()).await;
            }
            Err(e) => {
                if let Err(undo) = store.delete(&key).await {
                    tracing::warn!(key = %key, error = %undo, "could not remove a half-stored photo");
                } else {
                    uncounted(&state, &key).await;
                }
                return Err(storage_failed(e));
            }
        }
    }

    // A document is bytes until something reads it. Queued rather than done
    // here: a scanned file can take minutes, and the upload has already
    // succeeded -- the words arriving later is a file not ready yet, where a
    // request held open for them is a failure the uploader cannot act on.
    super::extract::enqueue(&state.pool, claims.workspace_id, &key, &scoped).await;

    // Putting a file into a conversation is using it. Session files are swept
    // once a session has been idle (`storage_sweep`), and one dropped into a
    // session swept before would otherwise never be swept at all; it also
    // brings the conversation up the recent list, where the person who just
    // added to it will look for it.
    if s == Scope::Session {
        let _ = sqlx::query(
            "update agent_sessions set last_active_at = now() \
             where id = $1 and workspace_id = $2",
        )
        .bind(session_id)
        .bind(claims.workspace_id)
        .execute(&state.pool)
        .await;
    }

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
            converted_from: original.map(|(kept, _)| kept),
        }),
    ))
}

/// The first of `names` with nothing stored under it.
///
/// Bounded, as anything looping on what a client can create should be: a
/// hundred photos of one name in one place is somebody's script, and is told
/// so rather than probed further.
async fn free_name(
    store: &dyn outturn_core::storage::StorageBackend,
    space: &Space,
    names: impl Iterator<Item = String>,
) -> Result<String, ApiError> {
    for name in names.take(100) {
        let key = scope::resolve(space, &name).map_err(storage_failed)?;
        match store.stat(&key).await {
            Err(outturn_core::storage::StorageError::NotFound) => return Ok(name),
            Ok(_) => continue,
            Err(e) => return Err(storage_failed(e)),
        }
    }
    Err((
        StatusCode::CONFLICT,
        "There are too many files of this name here already.".into(),
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
    let bytes = store
        .read(&key, 0, u32::MAX)
        .await
        .map_err(storage_failed)?;
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

/// The most a preview of an image reads: all of it, up to what an upload may
/// be.
///
/// The top of a text file is still worth reading; the top of a picture is a
/// strip of it with gray below. An ordinary photo or screenshot is well over
/// the text bound, so an image is served whole or not at all.
pub const MAX_IMAGE_PREVIEW_BYTES: usize = MAX_UPLOAD_BYTES;

/// How much of a file of this type a preview may show.
fn preview_bound(media_type: &str) -> usize {
    if media_type.starts_with("image/") {
        MAX_IMAGE_PREVIEW_BYTES
    } else {
        MAX_PREVIEW_BYTES
    }
}

/// What of `bytes` -- read to one past the bound -- a preview shows, and
/// whether that is less than the file.
///
/// `None` for an image over its bound: half a picture is not a preview of
/// it, so it is refused rather than cut.
fn fit<'a>(bytes: &'a [u8], media_type: &str) -> Option<(&'a [u8], bool)> {
    let bound = preview_bound(media_type);
    let truncated = bytes.len() > bound;
    if truncated && media_type.starts_with("image/") {
        return None;
    }
    Some((&bytes[..bytes.len().min(bound)], truncated))
}

/// What a preview may be served as, decided by the bytes rather than the name.
///
/// An allowlist and a short one. These files are uploaded by people and
/// written by agents, so serving one inline means running somebody else's
/// content on this origin -- and the session cookie that authorizes every API
/// call is on this origin. HTML and SVG are the obvious ways that goes wrong
/// and are deliberately absent: an SVG is a document that can carry script,
/// whatever its extension says.
///
/// The extension is never consulted. `notes.txt` holding a PNG is a PNG, and
/// `photo.png` holding HTML is refused rather than believed.
fn previewable(bytes: &[u8]) -> Option<&'static str> {
    if let Some(image) = outturn_core::vision::media_type(bytes) {
        return Some(image);
    }
    // Text, by the same reasoning `describe_binary` uses: something with a NUL
    // or a stretch of control characters is not prose, whatever it is called.
    if outturn_core::utf8::describe_binary(bytes).is_some() {
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
    let mut bytes = store
        .read(&key, 0, MAX_PREVIEW_BYTES as u32 + 1)
        .await
        .map_err(storage_failed)?;

    let head = &bytes[..bytes.len().min(MAX_PREVIEW_BYTES)];
    let Some(media_type) = previewable(head) else {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "this file has no preview".to_string(),
        ));
    };
    // An image past the text bound is read again, whole, to its own.
    if bytes.len() > MAX_PREVIEW_BYTES && preview_bound(media_type) > MAX_PREVIEW_BYTES {
        bytes = store
            .read(&key, 0, preview_bound(media_type) as u32 + 1)
            .await
            .map_err(storage_failed)?;
    }
    let Some((shown, truncated)) = fit(&bytes, media_type) else {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "this image is too large to preview".to_string(),
        ));
    };

    Ok((
        [
            (header::CONTENT_TYPE, media_type.to_string()),
            // Inline, which is the whole point, but under a sandbox: even if
            // something got through the allowlist it runs as its own origin
            // with no script, so it cannot reach the cookie that authorizes
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
    uncounted(&state, &key).await;
    tracing::info!(actor = %claims.subject, session_id = %session_id, path = %path, "file deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Records a stored object for the quota. Logged rather than failed: the
/// file is stored either way, and the weekly listing corrects the count.
async fn counted(state: &ApiState, key: &str, bytes: usize) {
    if let Err(e) = outturn_core::storage::stored::record(&state.pool, key, bytes as u64).await {
        tracing::warn!(key, error = %e, "could not count a stored file");
    }
}

async fn uncounted(state: &ApiState, key: &str) {
    if let Err(e) = outturn_core::storage::stored::forget(&state.pool, key).await {
        tracing::warn!(key, error = %e, "could not uncount a deleted file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_what_its_bytes_say_rather_than_what_it_is_called() {
        // The name comes from whoever uploaded it or whatever the agent wrote,
        // so it is a claim rather than a fact. Every decision here is made on
        // the bytes.
        assert_eq!(
            previewable(b"just some prose"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            previewable(b"\x89PNG\r\n\x1a\n and so on"),
            Some("image/png")
        );
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
    fn text_is_cut_at_its_bound_and_says_so() {
        let text = vec![b'a'; MAX_PREVIEW_BYTES + 1];
        let (shown, truncated) = fit(&text, "text/plain; charset=utf-8").unwrap();
        assert_eq!(shown.len(), MAX_PREVIEW_BYTES);
        assert!(truncated);

        let exact = vec![b'a'; MAX_PREVIEW_BYTES];
        assert_eq!(
            fit(&exact, "text/plain; charset=utf-8"),
            Some((&exact[..], false))
        );
    }

    #[test]
    fn an_image_is_shown_whole_or_not_at_all() {
        // A 400KB photo is past the text bound and nowhere near the image one,
        // so it comes back entire rather than as its top strip.
        let photo = vec![0u8; 400 * 1024];
        assert_eq!(fit(&photo, "image/jpeg"), Some((&photo[..], false)));

        let largest = vec![0u8; MAX_IMAGE_PREVIEW_BYTES];
        assert_eq!(fit(&largest, "image/png"), Some((&largest[..], false)));

        // Past its bound it is refused, never cut.
        let over = vec![0u8; MAX_IMAGE_PREVIEW_BYTES + 1];
        assert_eq!(fit(&over, "image/png"), None);
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
