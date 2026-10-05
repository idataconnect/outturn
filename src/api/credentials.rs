//! A workspace's sealed credentials: stored, listed, rotated and revoked.
//!
//! This tier never holds a plaintext. A credential arrives already sealed, by
//! the browser or `outturn-seal`, to the gateway's public key; what is checked
//! here is the binding it was sealed under, which is readable without opening
//! anything. The gateway checks it again on every use, against the exact bytes
//! stored, so nothing written here is the enforcement. See
//! docs/sealed-credentials.md.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use base64::Engine as _;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::Authority;
use crate::egress::seal::{self, Binding};

use super::router::{ApiError, ApiState, authorize};

/// The gateway's public key, from `OUTTURN_SEAL_PUBLIC_KEY`, with its id.
/// `None` when unset or unreadable: nothing can be sealed, and saying so beats
/// handing out a key that opens nowhere.
pub(super) fn seal_key_from_env() -> Option<(Vec<u8>, String)> {
    let hex_key = std::env::var("OUTTURN_SEAL_PUBLIC_KEY").ok()?;
    let bytes = hex::decode(hex_key.trim()).ok().filter(|b| b.len() == 32)?;
    let id = seal::key_id(&bytes);
    Some((bytes, id))
}

fn no_seal_key() -> ApiError {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "this deployment has no seal key, so credentials cannot be stored".into(),
    )
}

/// What a sealer needs: the key, its id, and the suite and label it is used
/// with, so a client never has to guess either.
#[derive(serde::Serialize)]
pub struct SealKey {
    pub public_key: String,
    pub key_id: String,
    pub suite: &'static str,
    pub info: &'static str,
}

pub async fn seal_key_for(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<SealKey>, ApiError> {
    authorize(&state, &headers, Authority::CredentialsWrite).await?;
    let (public, id) = state.seal_key.as_ref().ok_or_else(no_seal_key)?;
    Ok(Json(SealKey {
        public_key: hex::encode(public),
        key_id: id.clone(),
        suite: "HPKE base, DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, AES-256-GCM",
        info: std::str::from_utf8(seal::INFO).unwrap_or_default(),
    }))
}

/// A credential as a workspace sees it: where it may go, never what it is.
#[derive(Debug, serde::Serialize)]
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub binding: serde_json::Value,
    pub key_id: String,
    pub generation: i64,
    pub created_by: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn read(row: &sqlx::postgres::PgRow) -> Credential {
    let bytes: Vec<u8> = row.get("binding");
    Credential {
        id: row.get("id"),
        name: row.get("name"),
        binding: serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        key_id: row.get("key_id"),
        generation: row.get("generation"),
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        revoked_at: row.get("revoked_at"),
    }
}

/// The columns a credential is read from. A macro so the queries stay static
/// strings, which is what keeps them out of reach of input.
macro_rules! columns {
    () => {
        "id, name, binding, key_id, generation, created_by, created_at, updated_at, revoked_at"
    };
}

/// A seal as it arrives: the binding's exact bytes and the ciphertext, both
/// base64, and which key it was made to.
#[derive(Debug, serde::Deserialize)]
pub struct Sealed {
    pub binding: String,
    pub sealed: String,
    pub key_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct NewCredential {
    /// Chosen by whoever sealed it, because it is inside the binding and the
    /// binding is fixed before the seal is made.
    pub id: Uuid,
    pub name: String,
    #[serde(flatten)]
    pub seal: Sealed,
}

/// Reads and checks a seal for `workspace` and credential `id`.
///
/// Refuses a binding naming any workspace but the writer's own -- `"*"`
/// included, which is the operator's to write -- and a seal to a key this
/// deployment does not publish. Neither is the enforcement; both are said here
/// while somebody is looking.
fn check(
    state: &ApiState,
    workspace: Uuid,
    id: Uuid,
    seal: &Sealed,
) -> Result<(Vec<u8>, Vec<u8>), ApiError> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let engine = base64::engine::general_purpose::STANDARD;
    let binding = engine
        .decode(&seal.binding)
        .map_err(|_| bad("binding is not base64".into()))?;
    let sealed = engine
        .decode(&seal.sealed)
        .map_err(|_| bad("sealed is not base64".into()))?;
    let parsed = Binding::parse(&binding).map_err(bad)?;
    if parsed.credential != id {
        return Err(bad("the binding names a different credential".into()));
    }
    if parsed.workspaces != [workspace.to_string()] {
        return Err(bad(
            "a credential is bound to this workspace and no other".into()
        ));
    }
    let (_, current) = state.seal_key.as_ref().ok_or_else(no_seal_key)?;
    if &seal.key_id != current {
        return Err(bad(format!(
            "sealed to key {}, but this deployment's key is {current}",
            seal.key_id
        )));
    }
    if sealed.len() <= 32 {
        return Err(bad("sealed is too short to be a seal".into()));
    }
    Ok((binding, sealed))
}

pub async fn create(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<NewCredential>,
) -> Result<(StatusCode, Json<Credential>), ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    let name = input.name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err((StatusCode::BAD_REQUEST, "a credential needs a name".into()));
    }
    let (binding, sealed) = check(&state, claims.workspace_id, input.id, &input.seal)?;
    let row = sqlx::query(concat!(
        "insert into credentials (id, workspace_id, name, binding, sealed, key_id, created_by) \
         values ($1, $2, $3, $4, $5, $6, $7) returning ",
        columns!(),
        ""
    ))
    .bind(input.id)
    .bind(claims.workspace_id)
    .bind(name)
    .bind(&binding)
    .bind(&sealed)
    .bind(&input.seal.key_id)
    .bind(claims.subject)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => (
            StatusCode::CONFLICT,
            "a credential with that id already exists".to_string(),
        ),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    })?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        credential_id = %input.id,
        "credential stored"
    );
    Ok((StatusCode::CREATED, Json(read(&row))))
}

pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<super::PageQuery>,
) -> Result<Json<super::Page<Credential>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsRead).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows = sqlx::query(concat!(
        "select ",
        columns!(),
        " from credentials \
         where workspace_id = $1 and ($2::uuid is null or id > $2) order by id limit $3"
    ))
    .bind(claims.workspace_id)
    .bind(query.after)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let items: Vec<Credential> = rows.iter().map(read).collect();
    Ok(Json(super::Page::from_rows(items, limit, |c| c.id)))
}

/// Replaces a credential's seal, keeping its id: the key a client rotated at
/// their provider is the same credential to them, and every rule naming it
/// follows. Also how a revoked credential is given a key again.
pub async fn rotate(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<Sealed>,
) -> Result<Json<Credential>, ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    let (binding, sealed) = check(&state, claims.workspace_id, id, &input)?;
    let row = sqlx::query(concat!(
        "update credentials set binding = $3, sealed = $4, key_id = $5, \
             generation = generation + 1, updated_at = now(), revoked_at = null \
         where id = $1 and workspace_id = $2 returning ",
        columns!(),
        ""
    ))
    .bind(id)
    .bind(claims.workspace_id)
    .bind(&binding)
    .bind(&sealed)
    .bind(&input.key_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or((StatusCode::NOT_FOUND, "no such credential".to_string()))?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        credential_id = %id,
        "credential rotated"
    );
    Ok(Json(read(&row)))
}

/// Revokes a credential: the seal is wiped in the same statement and the
/// gateway told, so the next request finds nothing to attach.
///
/// That stops honest use. It does not bind anybody who can write the database,
/// since an old ciphertext restored from a backup still opens -- so a key that
/// was compromised is revoked at its provider too.
pub async fn revoke(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Credential>, ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    let row = sqlx::query(concat!(
        "update credentials set sealed = null, revoked_at = now(), \
             generation = generation + 1, updated_at = now() \
         where id = $1 and workspace_id = $2 and revoked_at is null returning ",
        columns!(),
        ""
    ))
    .bind(id)
    .bind(claims.workspace_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or((
        StatusCode::NOT_FOUND,
        "no such credential, or it is already revoked".to_string(),
    ))?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        credential_id = %id,
        "credential revoked"
    );
    Ok(Json(read(&row)))
}

/// The binding of a workspace's live credential, for checking a rule against
/// it when the rule is written.
pub async fn binding_of(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<Option<Binding>, sqlx::Error> {
    let row = sqlx::query(
        "select binding from credentials \
         where id = $1 and workspace_id = $2 and revoked_at is null",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| Binding::parse(&r.get::<Vec<u8>, _>("binding")).ok()))
}

#[derive(Debug, serde::Deserialize)]
pub struct TestCall {
    /// What to ask for, under the host: `/api/items`. A GET, and only a GET --
    /// this is a question about the key, and a test that wrote something would
    /// be an answer nobody asked for.
    #[serde(default)]
    pub path: Option<String>,
}

/// What the host said to one GET made with the credential.
#[derive(Debug, serde::Serialize)]
pub struct Tested {
    pub url: String,
    /// The host's own status: 200 means the key worked, 401 that it did not.
    pub status: u16,
    pub ok: bool,
    /// The gateway's fingerprint of the key it sent. The same key always shows
    /// the same one; a different one means the key is not the one it was.
    pub fingerprint: Option<String>,
}

#[derive(serde::Deserialize)]
struct Fetched {
    status: u16,
    #[serde(default)]
    credential_fingerprint: Option<String>,
}

/// Makes one GET through the gateway with the credential attached, exactly as
/// a turn would -- the same rule, the same commitment, the same checks -- and
/// says what the host answered.
///
/// So a wrong key is a red 401 here, while somebody is setting it up, rather
/// than an agent apologizing for it later. Only the status comes back: the
/// body is the account's data, and the question asked was whether the key
/// works.
pub async fn test(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<TestCall>,
) -> Result<Json<Tested>, ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    let path = input.path.unwrap_or_else(|| "/".into());
    if !path.starts_with('/') || path.starts_with("//") {
        return Err((
            StatusCode::BAD_REQUEST,
            "a test path starts with one /".into(),
        ));
    }
    let gateway = std::env::var("OUTTURN_GATEWAY_URL").map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "testing a credential needs OUTTURN_GATEWAY_URL set on the API".to_string(),
        )
    })?;
    // The workspace's own rule naming this credential: the test goes the way
    // a turn would, so a rule that is wrong fails here too.
    let rule = super::egress::rules_for(&state.pool, claims.workspace_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .into_iter()
        .find(|r| r.credential == Some(id))
        .ok_or((
            StatusCode::CONFLICT,
            "no egress rule sends this credential yet, so there is nothing to test".to_string(),
        ))?;
    let url = format!("https://{}{path}", rule.host);
    let rules = vec![rule];
    let gates = crate::egress::gate::Gates::none();
    let token = state
        .minter
        .mint_document_fetch(
            claims.subject,
            claims.workspace_id,
            crate::egress::commit::root(claims.workspace_id, &rules),
            gates.root(claims.workspace_id),
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let response = crate::http_client::reporting_client()
        .post(format!("{gateway}/v1/egress"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "method": "GET",
            "url": url,
            "gates": gates,
            "proof": crate::egress::commit::Proof::WholeSet { rules },
        }))
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("could not reach the gateway: {e}"),
            )
        })?;
    if !response.status().is_success() {
        // The gateway's refusals are written to be read.
        let detail = response.text().await.unwrap_or_default();
        return Err((StatusCode::UNPROCESSABLE_ENTITY, detail));
    }
    let fetched: Fetched = response.json().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("the gateway's answer was unreadable: {e}"),
        )
    })?;
    Ok(Json(Tested {
        url,
        status: fetched.status,
        ok: (200..300).contains(&fetched.status),
        fingerprint: fetched.credential_fingerprint,
    }))
}
