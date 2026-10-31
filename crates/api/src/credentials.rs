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

use outturn_core::auth::Authority;
use outturn_core::egress::seal::{self, Binding};

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
    seal_key_of(&state).map(Json)
}

/// The key and what a sealer needs beside it, for whoever may seal.
pub(super) fn seal_key_of(state: &ApiState) -> Result<SealKey, ApiError> {
    let (public, id) = state.seal_key.as_ref().ok_or_else(no_seal_key)?;
    Ok(SealKey {
        public_key: hex::encode(public),
        key_id: id.clone(),
        suite: "HPKE base, DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, AES-256-GCM",
        info: std::str::from_utf8(seal::INFO).unwrap_or_default(),
    })
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
    let (binding, parsed, sealed) = decode(state, id, seal)?;
    if parsed.workspaces != [workspace.to_string()] {
        return Err((
            StatusCode::BAD_REQUEST,
            "a credential is bound to this workspace and no other".into(),
        ));
    }
    // Storing a key as somebody is acting as them, so a person's own is
    // stored by them alone, never through these routes by whoever holds
    // `credentials:write`. See docs/user-credentials.md.
    if parsed.user.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "a person's own credential is connected by them, not stored for them".into(),
        ));
    }
    Ok((binding, sealed))
}

/// Reads a seal for credential `id`: its binding's bytes, the binding they
/// say, and the ciphertext. Refused when the binding names another
/// credential or the seal is to a key this deployment does not publish.
pub(super) fn decode(
    state: &ApiState,
    id: Uuid,
    seal: &Sealed,
) -> Result<(Vec<u8>, Binding, Vec<u8>), ApiError> {
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
    Ok((binding, parsed, sealed))
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
        sqlx::Error::Database(db)
            if db.is_unique_violation() && db.constraint() == Some("credentials_name_unique") =>
        {
            (
                StatusCode::CONFLICT,
                format!("there is already a credential named \"{name}\"; choose another name"),
            )
        }
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

/// The scheme, host and port a test is sent to: the caller's origin when its
/// host is the rule's, or `https://` and the rule's host.
///
/// Rules are kept without a port, so a host served on another one -- an
/// in-cluster service on 8080 -- was tested on 443 and refused, and the key
/// looked broken when the address was.
fn origin_for(origin: Option<&str>, host: &str) -> Result<String, ApiError> {
    let Some(origin) = origin else {
        return Ok(format!("https://{host}"));
    };
    let parsed = reqwest::Url::parse(origin).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("{origin} is not a URL: {e}"),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{origin} is not http or https"),
        ));
    }
    if parsed.host_str() != Some(host) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{origin} is not {host}, which is the host this key is for"),
        ));
    }
    Ok(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

#[derive(Debug, serde::Deserialize)]
pub struct TestCall {
    /// What to ask for, under the host: `/api/items`. A GET, and only a GET --
    /// this is a question about the key, and a test that wrote something would
    /// be an answer nobody asked for.
    #[serde(default)]
    pub path: Option<String>,
    /// Where the host is answered, `http://petstore:8080`, when that is not
    /// `https://` and the default port -- a generated skill's base URL says.
    /// Its host has to be the rule's: this picks the door, never the building.
    #[serde(default)]
    pub origin: Option<String>,
}

/// What the host said to one GET made with the credential.
/// What a test proved about the key, from asking twice: once with it and once
/// without. A 200 alone proves nothing -- plenty of paths answer anybody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Refused without the key and answered with it: the key is what let it in.
    Works,
    /// Answered without the key too, so this path says nothing about it.
    NotNeeded,
    /// Refused with the key.
    Refused,
    /// Anything else: an error, a redirect, or a path that does not exist.
    Unclear,
}

/// What the host said to one GET made with the credential, and to the same GET
/// made without it.
#[derive(Debug, serde::Serialize)]
pub struct Tested {
    pub url: String,
    /// The host's status with the key attached.
    pub status: u16,
    /// The host's status to the same request without the key, when it could
    /// be asked.
    pub without_key: Option<u16>,
    pub verdict: Verdict,
    /// The gateway's fingerprint of the key it sent. The same key always shows
    /// the same one; a different one means the key is not the one it was.
    pub fingerprint: Option<String>,
    /// The start of what the host said, when the key did not plainly work: a
    /// status alone leaves somebody guessing at a reason the host gave.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// A likely cause worth checking, where there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The first line of a body, short enough to show beside a status.
fn first_line(body: &str) -> Option<String> {
    let line = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(match line.char_indices().nth(200) {
        Some((cut, _)) => format!("{}...", &line[..cut]),
        None => line.to_string(),
    })
}

fn verdict(with_key: u16, without_key: Option<u16>) -> Verdict {
    let refused = |s: u16| s == 401 || s == 403;
    let fine = |s: u16| (200..300).contains(&s);
    match (with_key, without_key) {
        (k, _) if refused(k) => Verdict::Refused,
        (k, Some(b)) if fine(k) && refused(b) => Verdict::Works,
        (k, Some(b)) if fine(k) && fine(b) => Verdict::NotNeeded,
        _ => Verdict::Unclear,
    }
}

#[derive(serde::Deserialize)]
struct Fetched {
    status: u16,
    #[serde(default)]
    credential_fingerprint: Option<String>,
    /// What the host said, for a test that did not plainly work.
    #[serde(default)]
    body: String,
}

/// Makes a GET through the gateway with the credential attached, exactly as a
/// turn would -- the same rule, the same commitment, the same checks -- and the
/// same GET without it, and says what the pair proves about the key.
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
    // No default: the root of an API usually answers anybody, so testing it
    // proves nothing about a key. The caller names a path the key is for.
    let path = input.path.unwrap_or_default();
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
    let url = format!("{}{path}", origin_for(input.origin.as_deref(), &rule.host)?);
    // The same request with the credential taken off the rule, which is the
    // control: what the host says to somebody without the key.
    let mut bare = rule.clone();
    bare.credential = None;
    bare.credential_env = None;
    bare.header = None;

    // A service account's token is the gateway's to format, so a refusal says
    // nothing about how the key was pasted.
    let exchanged = binding_of(&state.pool, claims.workspace_id, id)
        .await
        .ok()
        .flatten()
        .is_some_and(|b| b.kind == outturn_core::egress::seal::Kind::ServiceAccount);
    let authorization = !exchanged
        && rule
            .header
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case("authorization"));
    let keyed = fetch(&state, &claims, &gateway, &url, rule).await?;
    let without_key = fetch(&state, &claims, &gateway, &url, bare)
        .await
        .ok()
        .map(|f| f.status);
    let verdict = verdict(keyed.status, without_key);
    // This tier never sees a sealed key, so it cannot tell a key pasted
    // without its scheme from a wrong one. It can say which to check first:
    // an Authorization key refused is most often a bare token where the API
    // wanted `Bearer` before it, which cost one connection a round to find.
    let hint = (verdict == Verdict::Refused && authorization).then(|| {
        "If the key was connected without its scheme, replace it with Bearer and a space \
         before the key."
            .to_string()
    });
    Ok(Json(Tested {
        said: (verdict != Verdict::Works && verdict != Verdict::NotNeeded)
            .then(|| first_line(&keyed.body))
            .flatten(),
        hint,
        verdict,
        url,
        status: keyed.status,
        without_key,
        fingerprint: keyed.credential_fingerprint,
    }))
}

/// One GET through the gateway under exactly one rule, as a turn would make it.
async fn fetch(
    state: &ApiState,
    claims: &outturn_core::auth::SessionClaims,
    gateway: &str,
    url: &str,
    rule: outturn_core::egress::rule::EgressRule,
) -> Result<Fetched, ApiError> {
    let rules = vec![rule];
    let gates = outturn_core::egress::gate::Gates::none();
    let token = state
        .minter
        .mint_document_fetch(
            claims.subject,
            claims.workspace_id,
            outturn_core::egress::commit::root(claims.workspace_id, &rules),
            gates.root(claims.workspace_id),
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let response = outturn_core::http_client::reporting_client()
        .post(format!("{gateway}/v1/egress"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "method": "GET",
            "url": url,
            "gates": gates,
            "proof": outturn_core::egress::commit::Proof::WholeSet { rules },
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
    response.json().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("the gateway's answer was unreadable: {e}"),
        )
    })
}

#[cfg(test)]
mod origin_tests {
    use super::origin_for;

    #[test]
    fn a_test_goes_to_the_port_the_skill_names_on_the_rules_host() {
        assert_eq!(
            origin_for(None, "books.example.com").unwrap(),
            "https://books.example.com"
        );
        assert_eq!(
            origin_for(Some("http://petstore:8080"), "petstore").unwrap(),
            "http://petstore:8080"
        );
        assert!(
            origin_for(Some("http://elsewhere:8080"), "petstore").is_err(),
            "another host"
        );
        assert!(origin_for(Some("ftp://petstore"), "petstore").is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::{Verdict, verdict};

    /// Only a refusal without the key and an answer with it proves the key.
    #[test]
    fn a_key_works_only_when_the_same_request_without_it_is_refused() {
        assert_eq!(verdict(200, Some(401)), Verdict::Works);
        assert_eq!(verdict(204, Some(403)), Verdict::Works);
    }

    /// The case that used to read as success: a path that answers anybody.
    #[test]
    fn a_path_that_answers_without_the_key_says_nothing_about_it() {
        assert_eq!(verdict(200, Some(200)), Verdict::NotNeeded);
    }

    #[test]
    fn a_refusal_with_the_key_is_a_refusal_whatever_the_control_said() {
        assert_eq!(verdict(401, Some(401)), Verdict::Refused);
        assert_eq!(verdict(403, None), Verdict::Refused);
    }

    /// A success with no control to compare it against is not proof either.
    #[test]
    fn anything_else_is_unclear() {
        assert_eq!(verdict(200, None), Verdict::Unclear);
        assert_eq!(verdict(404, Some(404)), Verdict::Unclear);
        assert_eq!(verdict(500, Some(401)), Verdict::Unclear);
        assert_eq!(verdict(200, Some(404)), Verdict::Unclear);
    }
}
