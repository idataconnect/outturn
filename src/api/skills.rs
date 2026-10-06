use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use crate::auth::Authority;

use super::files::{storage, storage_failed};
use super::router::{ApiError, ApiState, authorize};
use super::skill::{
    Binding, CreateSkill, ForkSkill, NewFile, NewVersion, Skill, SkillFile, SkillVersion,
    UpdateSkill, VersionSummary, blob_key, link, prepare,
};

/// Uploads a version's files under the workspace that will own it, before the
/// rows naming them are written: a failure between the two leaves a blob
/// nothing references, never a row naming a blob that is not there.
async fn store_files(
    state: &ApiState,
    owner: Uuid,
    files: Vec<NewFile>,
) -> Result<(Vec<SkillFile>, Vec<super::skill::DeclaredGate>), ApiError> {
    let prepared = prepare(files)?;
    if prepared.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // Read before the content goes, since that is the only moment it is in hand:
    // afterwards the bytes are in the object store under a hash, and parsing a
    // declaration would mean fetching them back. See `docs/approvals.md`.
    let gates = super::skill::declared_gates(&prepared);
    let store = storage(state)?;
    for (file, bytes) in &prepared {
        store
            .write(&blob_key(owner, &file.sha256), bytes)
            .await
            .map_err(storage_failed)?;
    }
    Ok((prepared.into_iter().map(|(f, _)| f).collect(), gates))
}

/// Works out the links of a version published before they were recorded, from
/// the content in the object store, and records them so this happens once.
///
/// A version left as it was says `unreached: null`, which a caller must read as
/// "not known"; this is what turns that into an answer.
async fn with_links(
    state: &ApiState,
    owner: Uuid,
    version: SkillVersion,
) -> Result<SkillVersion, ApiError> {
    if version.unreached.is_some() {
        return Ok(version);
    }
    let store = storage(state)?;
    let mut read = Vec::with_capacity(version.files.len());
    for f in &version.files {
        let bytes = store
            .read(&blob_key(owner, &f.sha256), 0, f.bytes as u32)
            .await
            .map_err(storage_failed)?;
        read.push((f.clone(), bytes));
    }
    link(&mut read);
    let files: Vec<SkillFile> = read.into_iter().map(|(f, _)| f).collect();
    state.skills.record_links(version.id, &files).await?;
    Ok(SkillVersion { files, ..version }.with_unreached())
}

/// `create` for either owner.
pub(super) async fn create_in(
    state: &ApiState,
    workspace: Uuid,
    author: Uuid,
    mut input: CreateSkill,
) -> Result<Skill, ApiError> {
    let (files, gates) = store_files(state, workspace, std::mem::take(&mut input.files)).await?;
    Ok(state
        .skills
        .create(workspace, author, input, &files, &gates)
        .await?)
}

/// `add_version` for either owner, answering 200 rather than 201 when nothing
/// changed and no version was appended.
pub(super) async fn add_version_in(
    state: &ApiState,
    workspace: Uuid,
    id: Uuid,
    author: Uuid,
    mut input: NewVersion,
) -> Result<(StatusCode, SkillVersion), ApiError> {
    let (files, gates) = match input.files.take() {
        Some(f) => {
            let (files, gates) = store_files(state, workspace, f).await?;
            (Some(files), gates)
        }
        // No files of its own, so no declarations of its own: the previous
        // version's files carry forward and their gates with them.
        None => (None, Vec::new()),
    };
    let (version, appended) = state
        .skills
        .add_version(workspace, id, author, input, files.as_deref(), &gates)
        .await?;
    let status = if appended {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, version))
}

/// The workspace's own skills and the operator's, which it may override.
pub async fn list_skills(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<super::PageQuery>,
) -> Result<Json<super::Page<Skill>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let items = state
        .skills
        .list(claims.workspace_id, query.after, limit + 1)
        .await?;
    Ok(Json(super::Page::from_rows(items, limit, |s| s.id)))
}

pub async fn create_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateSkill>,
) -> Result<(StatusCode, Json<Skill>), ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsWrite).await?;
    let skill = create_in(&state, claims.workspace_id, claims.subject, input).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        skill_id = %skill.id,
        kind = skill.kind.as_str(),
        "skill created"
    );
    Ok((StatusCode::CREATED, Json(skill)))
}

pub async fn get_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Skill>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    Ok(Json(state.skills.get(claims.workspace_id, id).await?))
}

pub async fn update_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateSkill>,
) -> Result<Json<Skill>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsWrite).await?;
    Ok(Json(
        state.skills.update(claims.workspace_id, id, input).await?,
    ))
}

pub async fn delete_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsWrite).await?;
    state.skills.delete(claims.workspace_id, id).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        skill_id = %id,
        "skill deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Withdraws a skill, or brings it back.
pub async fn retire_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<RetireSkill>,
) -> Result<Json<Skill>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsWrite).await?;
    Ok(Json(
        state
            .skills
            .retire(claims.workspace_id, id, input.retired)
            .await?,
    ))
}

#[derive(serde::Deserialize)]
pub struct RetireSkill {
    pub retired: bool,
}

pub async fn list_versions(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<super::PageQuery>,
) -> Result<Json<super::Page<VersionSummary>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let items = state
        .skills
        .versions(claims.workspace_id, id, query.after, limit + 1)
        .await?;
    // Newest first, so the version each one followed is the next; the row read
    // past the page is what the last one on it is compared with.
    let summaries = items
        .iter()
        .enumerate()
        .map(|(at, v)| VersionSummary::of(v, items.get(at + 1)))
        .collect();
    Ok(Json(super::Page::from_rows(summaries, limit, |v| v.id)))
}

pub async fn get_version(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<SkillVersion>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    let owner = state
        .skills
        .get(claims.workspace_id, id)
        .await?
        .workspace_id;
    let version = state
        .skills
        .version(claims.workspace_id, id, version_id)
        .await?;
    Ok(Json(with_links(&state, owner, version).await?))
}

/// One file of a version, as it was published.
pub async fn get_version_file(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((id, version_id, path)): Path<(Uuid, Uuid, String)>,
) -> Result<Response, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    let skill = state.skills.get(claims.workspace_id, id).await?;
    let version = state
        .skills
        .version(claims.workspace_id, id, version_id)
        .await?;
    let file = version.files.iter().find(|f| f.path == path).ok_or((
        StatusCode::NOT_FOUND,
        format!("no file {path} in this version"),
    ))?;
    let bytes = storage(&state)?
        .read(
            &blob_key(skill.workspace_id, &file.sha256),
            0,
            file.bytes as u32,
        )
        .await
        .map_err(storage_failed)?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], bytes).into_response())
}

/// Publishes a new version, which is the only way prose changes.
///
/// A rollback comes through here too, carrying the old body forward: there is
/// no endpoint that moves a skill backwards, because the history would then
/// have to be read in two directions to say what was live when.
pub async fn add_version(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<NewVersion>,
) -> Result<(StatusCode, Json<SkillVersion>), ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsWrite).await?;
    let (status, version) =
        add_version_in(&state, claims.workspace_id, id, claims.subject, input).await?;
    if status == StatusCode::CREATED {
        tracing::info!(
            actor = %claims.subject,
            workspace_id = %claims.workspace_id,
            skill_id = %id,
            ordinal = version.ordinal,
            "skill version published"
        );
    }
    Ok((status, Json(version)))
}

/// Takes a copy of somebody else's skill, keeping only a note of where it came
/// from. Nothing is merged back afterwards.
pub async fn fork_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<ForkSkill>,
) -> Result<(StatusCode, Json<Skill>), ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsWrite).await?;

    // The copy's files must be this workspace's to keep, so an operator's
    // skill forked here has its content copied rather than referenced.
    let source = state.skills.get(claims.workspace_id, id).await?;
    let version_id = input
        .version_id
        .or(source.version_id)
        .ok_or((StatusCode::NOT_FOUND, "skill has no version".to_string()))?;
    let taken = state
        .skills
        .version(claims.workspace_id, id, version_id)
        .await?;
    if source.workspace_id != claims.workspace_id && !taken.files.is_empty() {
        let store = storage(&state)?;
        for f in &taken.files {
            let bytes = store
                .read(&blob_key(source.workspace_id, &f.sha256), 0, f.bytes as u32)
                .await
                .map_err(storage_failed)?;
            store
                .write(&blob_key(claims.workspace_id, &f.sha256), &bytes)
                .await
                .map_err(storage_failed)?;
        }
    }

    let skill = state
        .skills
        .fork(claims.workspace_id, id, claims.subject, input)
        .await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        skill_id = %skill.id,
        forked_from = %id,
        "skill forked"
    );
    Ok((StatusCode::CREATED, Json(skill)))
}

/// Opens the hosts a skill declares and this workspace has not allowed.
///
/// Gated on the authority that writes an egress rule, not the one that writes
/// skills: otherwise a role allowed to author skills but not to open the
/// network could grant itself network access by declaring a host and installing
/// its own skill. Authoring is not consent, and the consent has to come from
/// somebody who could have written the rule by hand.
pub async fn approve_skill_hosts(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<String>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SettingsUpdate).await?;
    Ok(Json(
        state
            .skills
            .approve_hosts(claims.workspace_id, id, claims.subject)
            .await?,
    ))
}

pub async fn list_agent_skills(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(agent_id): Path<Uuid>,
) -> Result<Json<Vec<Binding>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    // Proves the agent is this workspace's before answering for it.
    state.agents.get(claims.workspace_id, agent_id).await?;
    Ok(Json(
        state.skills.bindings(claims.workspace_id, agent_id).await?,
    ))
}

pub async fn set_agent_skills(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(agent_id): Path<Uuid>,
    Json(input): Json<Vec<Binding>>,
) -> Result<Json<Vec<Binding>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    state.agents.get(claims.workspace_id, agent_id).await?;
    state
        .skills
        .set_bindings(claims.workspace_id, agent_id, &input)
        .await?;
    Ok(Json(
        state.skills.bindings(claims.workspace_id, agent_id).await?,
    ))
}

// OpenAPI wizard --------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct WizardRequest {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub base_url: String,
    /// The credential header the egress rule will carry; see
    /// `WizardInput::auth_header`.
    #[serde(default)]
    pub auth_header: Option<String>,
    /// See `spec_bytes`.
    pub spec: serde_json::Value,
}

/// A specification as the browser sent it: a string is the document's own
/// text, JSON or YAML, read as the wizard reads any document; anything else
/// is a JSON value already parsed.
pub(super) fn spec_bytes(spec: serde_json::Value) -> Result<Vec<u8>, ApiError> {
    match spec {
        serde_json::Value::String(text) => Ok(text.into_bytes()),
        other => serde_json::to_vec(&other)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid spec: {e}"))),
    }
}

/// A specification inline, or the URL to fetch one from. Exactly one.
#[derive(serde::Deserialize)]
pub struct PreviewRequest {
    #[serde(default)]
    pub spec: Option<serde_json::Value>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(serde::Serialize)]
pub struct PreviewResponse {
    #[serde(flatten)]
    pub preview: super::skill::wizard::Preview,
    /// The specification as fetched, when it was named by URL, so the create
    /// that follows sends what was previewed rather than fetching again and
    /// perhaps getting something else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<serde_json::Value>,
}

/// What the specification answers of the form, before anything is created.
/// Operator-only like the create beside it, since nobody else can use the
/// answer.
pub async fn preview_platform_skill_from_openapi(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PreviewRequest>,
) -> Result<Json<PreviewResponse>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    as_operator(&claims)?;
    let (spec_json, fetched_from) = match (req.spec, req.url) {
        (Some(spec), None) => (spec_bytes(spec)?, None),
        (None, Some(url)) => (
            fetch_document(&state, claims.subject, &url).await?,
            Some(url),
        ),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "send a specification or a URL to fetch one from, not both".into(),
            ));
        }
    };
    let preview = super::skill::wizard::preview(&spec_json, fetched_from.as_deref())
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    // Sent back as JSON whatever it was fetched as, so the create that
    // follows reads the same document without asking which. Parsed already
    // by `preview`, so this cannot fail on a document it accepted.
    let spec = match fetched_from {
        Some(_) => Some(
            super::skill::wizard::document(&spec_json)
                .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?,
        ),
        None => None,
    };
    Ok(Json(PreviewResponse { preview, spec }))
}

#[derive(serde::Deserialize)]
struct Fetched {
    status: u16,
    body: String,
    truncated: bool,
}

/// Fetches a specification through the gateway, the way an agent's
/// `fetch_url` reaches a host -- never with a client of the API's own, which
/// would be a URL field that reaches `outturn-api` and everything else in the
/// cluster.
///
/// The API has no turn to borrow a token from, so it mints one for this
/// request: committing to the one host the URL names, with no credential, no
/// gates, the `document_fetch` role and a minute to live. The gateway then
/// does what it does for every request -- refuses a private address unless
/// the operator opened it, pins what it resolved, follows no redirect -- and
/// the token is dropped here. Unauthenticated by construction: the rule names
/// no credential, so none is attached.
async fn fetch_document(state: &ApiState, actor: Uuid, url: &str) -> Result<Vec<u8>, ApiError> {
    let gateway = std::env::var("OUTTURN_GATEWAY_URL").map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "fetching by URL needs OUTTURN_GATEWAY_URL set on the API; upload the file instead"
                .to_string(),
        )
    })?;
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("that URL is not one: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err((
            StatusCode::BAD_REQUEST,
            "only http and https URLs are fetched".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or((
            StatusCode::BAD_REQUEST,
            "that URL names no host".to_string(),
        ))?
        .to_string();

    let workspace = crate::api::usage::PLATFORM_WORKSPACE;
    let rules = vec![crate::runtime::egress::EgressRule {
        host,
        header: None,
        credential_env: None,
        client: None,
        credential: None,
    }];
    let gates = crate::egress::gate::Gates::none();
    let token = state
        .minter
        .mint_document_fetch(
            actor,
            workspace,
            crate::egress::commit::root(workspace, &rules),
            gates.root(workspace),
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let response = crate::http_client::reporting_client()
        .post(format!("{gateway}/v1/egress"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "method": "GET",
            "url": url,
            "gates": gates,
            "headers": [[
                "accept",
                "application/json, application/yaml, text/yaml;q=0.9, */*;q=0.1",
            ]],
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
    let status = response.status();
    if !status.is_success() {
        // The gateway's refusals are written to be read: a private address,
        // an unresolvable name, a host that did not answer.
        let detail = response.text().await.unwrap_or_default();
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("could not fetch {url}: {detail}"),
        ));
    }
    let fetched: Fetched = response.json().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("the gateway's answer was unreadable: {e}"),
        )
    })?;
    if !(200..300).contains(&fetched.status) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{url} answered {}", fetched.status),
        ));
    }
    if fetched.truncated {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{url} is larger than a specification may be"),
        ));
    }
    tracing::info!(actor = %actor, %url, bytes = fetched.body.len(), "specification fetched");
    Ok(fetched.body.into_bytes())
}

pub async fn create_platform_skill_from_openapi(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<WizardRequest>,
) -> Result<(StatusCode, Json<Skill>), ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let workspace = as_operator(&claims)?;

    let spec_json = spec_bytes(req.spec)?;
    let base_url = req.base_url.clone();
    let auth_header = req.auth_header.clone();

    let input = super::skill::wizard::WizardInput {
        spec_json,
        slug: req.slug.clone(),
        base_url: req.base_url,
        auth_header: req.auth_header,
        annotations: Vec::new(),
    };
    let output = super::skill::wizard::generate(&input)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;

    let file_count = output.files.len();
    let files: Vec<NewFile> = output
        .files
        .into_iter()
        .map(|(path, content)| NewFile { path, content })
        .collect();

    let create = CreateSkill {
        slug: req.slug,
        name: req.name,
        description: req.description,
        body: output.body,
        base_skill_id: None,
        hosts: output.hosts,
        files,
    };

    let skill = create_in(&state, workspace, claims.subject, create).await?;
    // Kept, so the skill can be made again with what people add to it.
    let revision = super::skill_sources::record(
        &state,
        workspace,
        skill.id,
        claims.subject,
        base_url.trim().trim_end_matches('/'),
        auth_header
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty()),
        &input.spec_json,
    )
    .await?;
    sqlx::query("update skill_versions set source_revision_id = $2, annotation_ids = '{}' where skill_id = $1")
        .bind(skill.id)
        .bind(revision)
        .execute(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(
        actor = %claims.subject,
        skill_id = %skill.id,
        file_count,
        "platform skill created from openapi"
    );
    Ok((StatusCode::CREATED, Json(skill)))
}

// The operator's own skills ----------------------------------------------------
//
// Reads need no platform route: a workspace already sees the operator's skills
// beside its own, since it cannot decide whether to override what it cannot
// see. Writes do, because they land in the platform workspace rather than the
// caller's, and only the operator may put anything there.

/// The operator writes at the platform level, and nobody else does. Gated the
/// way platform settings are: a system administrator by role, not an authority
/// a workspace could be granted.
pub(super) fn as_operator(claims: &crate::auth::SessionClaims) -> Result<Uuid, ApiError> {
    if !claims.is_system_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator writes platform skills".into(),
        ));
    }
    Ok(crate::api::usage::PLATFORM_WORKSPACE)
}

pub async fn create_platform_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateSkill>,
) -> Result<(StatusCode, Json<Skill>), ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let workspace = as_operator(&claims)?;
    let skill = create_in(&state, workspace, claims.subject, input).await?;
    tracing::info!(actor = %claims.subject, skill_id = %skill.id, "platform skill created");
    Ok((StatusCode::CREATED, Json(skill)))
}

pub async fn update_platform_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateSkill>,
) -> Result<Json<Skill>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let workspace = as_operator(&claims)?;
    Ok(Json(state.skills.update(workspace, id, input).await?))
}

pub async fn add_platform_version(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<NewVersion>,
) -> Result<(StatusCode, Json<SkillVersion>), ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let workspace = as_operator(&claims)?;
    let (status, version) = add_version_in(&state, workspace, id, claims.subject, input).await?;
    if status == StatusCode::CREATED {
        tracing::info!(
            actor = %claims.subject,
            skill_id = %id,
            ordinal = version.ordinal,
            "platform skill version published"
        );
    }
    Ok((status, Json(version)))
}

/// Withdrawing an operator's skill leaves every workspace already using it
/// working, which is the point of retiring rather than deleting: a delete would
/// be refused anyway by any override standing on it.
/// Deletes one of the operator's skills that nothing has used: never given to
/// an agent, never run in a turn. Anything else is retired, which keeps its
/// history.
pub async fn delete_platform_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let workspace = as_operator(&claims)?;
    state.skills.delete(workspace, id).await?;
    tracing::info!(actor = %claims.subject, skill_id = %id, "platform skill deleted");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn retire_platform_skill(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<RetireSkill>,
) -> Result<Json<Skill>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let workspace = as_operator(&claims)?;
    Ok(Json(
        state.skills.retire(workspace, id, input.retired).await?,
    ))
}
