//! Files a person sends in pieces, for anything too large for one request.
//!
//! See `docs/large-uploads.md`. Each piece passes through here to the store as
//! one part of a multipart upload, so this tier holds at most one piece per
//! request and needs nothing of the store a single upload does not: a turnkey
//! deployment's store inside the cluster works as shipped. The store keeps the
//! pieces; what is kept here is one row per upload, for the quota and for
//! resuming, and each piece's tag, which completing needs and the store cannot
//! be asked for by this client.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use outturn_core::storage::scope::{self, Scope};

use super::files::{
    StoredFile, counted, landed, parse_scope, space_for, storage, storage_failed, write_authority,
};
use super::router::{ApiError, ApiState, authorize};

/// The size of every piece but the last. Above the store's five-mebibyte
/// minimum, and small enough that a piece retried after a drop costs little.
pub const PIECE_BYTES: u64 = 16 * 1024 * 1024;

/// The most one file sent in pieces may be. Well inside what the store allows
/// (10,000 pieces), and a bound on what one person can reserve of a quota by
/// saying a size.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// How long an upload may be left half done: the bucket drops pieces nobody
/// completed a day after the upload began (`S3Storage::ensure_lifecycle`).
const OPEN_HOURS: i32 = 24;

#[derive(Debug, Deserialize)]
pub struct Start {
    /// `session`, `agent` or `workspace`.
    pub scope: String,
    /// The file's path under the scope, as a single upload names it.
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct Started {
    pub upload_id: Uuid,
    pub piece_size: u64,
    pub pieces: u32,
}

#[derive(Debug, Serialize)]
pub struct Progress {
    pub upload_id: Uuid,
    pub path: String,
    pub size: u64,
    pub piece_size: u64,
    pub pieces: u32,
    /// The pieces the store has, by number.
    pub received: Vec<u32>,
    pub state: String,
}

/// How many pieces a file of `size` is sent in.
fn pieces(size: u64) -> u32 {
    size.div_ceil(PIECE_BYTES) as u32
}

/// How large piece `number` of a file of `size` must be: every piece the same
/// but the last, which is what remains.
fn piece_len(size: u64, number: u32) -> Option<u64> {
    let count = pieces(size);
    if number == 0 || number > count {
        return None;
    }
    Some(if number < count {
        PIECE_BYTES
    } else {
        size - PIECE_BYTES * (count as u64 - 1)
    })
}

/// Begins an upload: checked as a single upload is, and against the quota
/// for the whole size before a byte is sent.
pub async fn start(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(session_id): Path<Uuid>,
    Json(input): Json<Start>,
) -> Result<(StatusCode, Json<Started>), ApiError> {
    let s = parse_scope(&input.scope)?;
    let claims = authorize(&state, &headers, write_authority(s)).await?;
    let space = space_for(&state, &claims, session_id, write_authority(s)).await?;
    let store = storage(&state)?;
    if input.size == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "an empty file is sent in one request".into(),
        ));
    }
    if input.size > MAX_FILE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("the largest file that can be sent is {MAX_FILE_BYTES} bytes"),
        ));
    }
    let scoped = format!("{}/{}", s.as_str(), input.path.trim_start_matches('/'));
    let key = scope::resolve(&space, &scoped).map_err(storage_failed)?;
    super::quota::check(
        &state,
        claims.workspace_id,
        std::slice::from_ref(&key),
        input.size,
    )
    .await?;

    let store_upload_id = store.start_upload(&key).await.map_err(storage_failed)?;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into uploads \
           (id, workspace_id, session_id, user_id, path, key, size, store_upload_id, expires_at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(hours => $9))",
    )
    .bind(id)
    .bind(claims.workspace_id)
    .bind(session_id)
    .bind(claims.subject)
    .bind(&scoped)
    .bind(&key)
    .bind(input.size as i64)
    .bind(&store_upload_id)
    .bind(OPEN_HOURS)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    Ok((
        StatusCode::CREATED,
        Json(Started {
            upload_id: id,
            piece_size: PIECE_BYTES,
            pieces: pieces(input.size),
        }),
    ))
}

/// An upload as this tier keeps it.
#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    path: String,
    key: String,
    size: i64,
    store_upload_id: String,
    state: String,
    expired: bool,
}

/// The caller's own upload into this session, checked again against what
/// they may write: an upload outlives the request that began it, and so may
/// outlive the role that allowed it.
async fn own(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
    session_id: Uuid,
    upload_id: Uuid,
) -> Result<(outturn_core::auth::SessionClaims, Scope, Row), ApiError> {
    // Whose it is first, then whether they may still write where it lands.
    let claims = super::router::authenticate(state, headers)?;
    let row: Row = sqlx::query_as(
        "select id, path, key, size, store_upload_id, state, expires_at < now() as expired \
         from uploads where id = $1 and session_id = $2 and workspace_id = $3 and user_id = $4",
    )
    .bind(upload_id)
    .bind(session_id)
    .bind(claims.workspace_id)
    .bind(claims.subject)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or((StatusCode::NOT_FOUND, "no such upload".to_string()))?;
    let s = parse_scope(row.path.split('/').next().unwrap_or_default())?;
    let claims = authorize(state, headers, write_authority(s)).await?;
    space_for(state, &claims, session_id, write_authority(s)).await?;
    Ok((claims, s, row))
}

fn still_open(row: &Row) -> Result<(), ApiError> {
    match row.state.as_str() {
        "open" if !row.expired => Ok(()),
        "open" => Err((
            StatusCode::GONE,
            "this upload was left too long and has to be started again".into(),
        )),
        "complete" => Err((
            StatusCode::CONFLICT,
            "this upload is already complete".into(),
        )),
        _ => Err((StatusCode::GONE, "this upload was abandoned".into())),
    }
}

/// Stores one piece. Sending a piece again replaces it, so a piece whose
/// answer was lost is simply sent again.
pub async fn piece(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, upload_id, number)): Path<(Uuid, Uuid, u32)>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let (_, _, row) = own(&state, &headers, session_id, upload_id).await?;
    still_open(&row)?;
    let size = row.size as u64;
    let Some(want) = piece_len(size, number) else {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "a file of this size is sent in pieces 1 to {}",
                pieces(size)
            ),
        ));
    };
    if body.len() as u64 != want {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "piece {number} must be {want} bytes, and this was {}",
                body.len()
            ),
        ));
    }
    let store = storage(&state)?;
    let etag = store
        .put_upload_part(&row.key, &row.store_upload_id, number, body.to_vec())
        .await
        .map_err(storage_failed)?;
    sqlx::query(
        "insert into upload_parts (upload_id, number, size, etag) values ($1, $2, $3, $4) \
         on conflict (upload_id, number) do update set size = excluded.size, etag = excluded.etag",
    )
    .bind(row.id)
    .bind(number as i32)
    .bind(want as i64)
    .bind(&etag)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn received(state: &ApiState, upload_id: Uuid) -> Result<Vec<(u32, String)>, ApiError> {
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "select number, etag from upload_parts where upload_id = $1 order by number",
    )
    .bind(upload_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(rows.into_iter().map(|(n, e)| (n as u32, e)).collect())
}

/// What has arrived, so a browser that lost its place sends only the rest.
pub async fn progress(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, upload_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Progress>, ApiError> {
    let (_, _, row) = own(&state, &headers, session_id, upload_id).await?;
    let received = received(&state, row.id).await?;
    let state_name = if row.state == "open" && row.expired {
        "expired".to_string()
    } else {
        row.state.clone()
    };
    Ok(Json(Progress {
        upload_id: row.id,
        path: row.path,
        size: row.size as u64,
        piece_size: PIECE_BYTES,
        pieces: pieces(row.size as u64),
        received: received.into_iter().map(|(n, _)| n).collect(),
        state: state_name,
    }))
}

/// Joins the pieces into the file. Whatever was at the path is replaced only
/// now, when the new file is whole.
pub async fn complete(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, upload_id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<StoredFile>), ApiError> {
    let (claims, s, row) = own(&state, &headers, session_id, upload_id).await?;
    still_open(&row)?;
    let size = row.size as u64;
    let parts = received(&state, row.id).await?;
    let missing: Vec<u32> = (1..=pieces(size))
        .filter(|n| !parts.iter().any(|(got, _)| got == n))
        .collect();
    if !missing.is_empty() {
        return Err((
            StatusCode::CONFLICT,
            format!("{} pieces have not arrived yet", missing.len()),
        ));
    }

    // Claimed before the store is asked, so two completions of one upload
    // cannot both land it.
    let claimed =
        sqlx::query("update uploads set state = 'complete' where id = $1 and state = 'open'")
            .bind(row.id)
            .execute(&state.pool)
            .await
            .map_err(internal)?;
    if claimed.rows_affected() == 0 {
        return Err((
            StatusCode::CONFLICT,
            "this upload is already complete".into(),
        ));
    }

    let store = storage(&state)?;
    if let Err(e) = store
        .complete_upload(&row.key, &row.store_upload_id, &parts)
        .await
    {
        // The pieces are still the store's, so the upload goes back to open
        // and can be completed again.
        let _ = sqlx::query("update uploads set state = 'open' where id = $1")
            .bind(row.id)
            .execute(&state.pool)
            .await;
        return Err(storage_failed(e));
    }
    super::extract::invalidate(store.as_ref(), &row.key).await;

    // A phone's photo is converted when it arrives in one request; one large
    // enough to need pieces is not a photo anybody took, and is not stored as
    // one either.
    let head = store.read(&row.key, 0, 16).await.unwrap_or_default();
    if super::heic::is_heif(&head) {
        let _ = store.delete(&row.key).await;
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "a HEIC photo is uploaded as one file, where it is converted".into(),
        ));
    }

    counted(&state, &row.key, size as usize).await;
    landed(
        &state,
        claims.workspace_id,
        s,
        session_id,
        &row.key,
        &row.path,
    )
    .await;
    tracing::info!(
        actor = %claims.subject,
        session_id = %session_id,
        path = %row.path,
        bytes = size,
        "file uploaded in pieces"
    );
    Ok((
        StatusCode::CREATED,
        Json(StoredFile {
            path: row.path,
            scope: s.as_str(),
            size,
            converted_from: None,
        }),
    ))
}

/// Gives up on an upload, and has the store drop its pieces.
pub async fn abandon(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((session_id, upload_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let (_, _, row) = own(&state, &headers, session_id, upload_id).await?;
    if row.state != "open" {
        return Ok(StatusCode::NO_CONTENT);
    }
    sqlx::query("update uploads set state = 'abandoned' where id = $1 and state = 'open'")
        .bind(row.id)
        .execute(&state.pool)
        .await
        .map_err(internal)?;
    if let Ok(store) = storage(&state)
        && let Err(e) = store.abort_upload(&row.key, &row.store_upload_id).await
    {
        // The lifecycle rule drops them within a day regardless.
        tracing::warn!(error = %e, "could not drop an abandoned upload's pieces");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Marks uploads left past their day as abandoned and drops their pieces.
/// Run with the storage sweep.
pub async fn sweep_expired(
    pool: &sqlx::PgPool,
    store: &Arc<dyn outturn_core::storage::StorageBackend>,
) -> Result<usize, sqlx::Error> {
    let expired: Vec<(String, String)> = sqlx::query_as(
        "update uploads set state = 'abandoned' \
         where id in (select id from uploads where state = 'open' and expires_at < now() \
                      limit 100) \
         returning key, store_upload_id",
    )
    .fetch_all(pool)
    .await?;
    for (key, store_upload_id) in &expired {
        let _ = store.abort_upload(key, store_upload_id).await;
    }
    Ok(expired.len())
}

fn internal(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "an upload could not be recorded");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "the upload could not be recorded".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_piece_is_the_same_but_the_last() {
        let size = PIECE_BYTES * 2 + 5;
        assert_eq!(pieces(size), 3);
        assert_eq!(piece_len(size, 1), Some(PIECE_BYTES));
        assert_eq!(piece_len(size, 2), Some(PIECE_BYTES));
        assert_eq!(piece_len(size, 3), Some(5));
        assert_eq!(piece_len(size, 0), None);
        assert_eq!(piece_len(size, 4), None);
        // A file of exactly so many pieces has no short one.
        assert_eq!(pieces(PIECE_BYTES * 2), 2);
        assert_eq!(piece_len(PIECE_BYTES * 2, 2), Some(PIECE_BYTES));
    }
}
