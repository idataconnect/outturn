use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use crate::auth::Authority;

use super::files::{storage, storage_failed};
use super::router::{ApiError, ApiState, authorize};
use super::skill::{
    Binding, CreateSkill, ForkSkill, NewFile, NewVersion, Skill, SkillFile, SkillVersion,
    UpdateSkill, blob_key, prepare,
};

/// Uploads a version's files under the workspace that will own it, before the
/// rows naming them are written: a failure between the two leaves a blob
/// nothing references, never a row naming a blob that is not there.
async fn store_files(
    state: &ApiState,
    owner: Uuid,
    files: Vec<NewFile>,
) -> Result<Vec<SkillFile>, ApiError> {
    let prepared = prepare(files)?;
    if prepared.is_empty() {
        return Ok(Vec::new());
    }
    let store = storage(state)?;
    for (file, bytes) in &prepared {
        store
            .write(&blob_key(owner, &file.sha256), 0, bytes)
            .await
            .map_err(storage_failed)?;
    }
    Ok(prepared.into_iter().map(|(f, _)| f).collect())
}

/// `create` for either owner.
async fn create_in(
    state: &ApiState,
    workspace: Uuid,
    author: Uuid,
    mut input: CreateSkill,
) -> Result<Skill, ApiError> {
    let files = store_files(state, workspace, std::mem::take(&mut input.files)).await?;
    Ok(state
        .skills
        .create(workspace, author, input, &files)
        .await?)
}

/// `add_version` for either owner, answering 200 rather than 201 when nothing
/// changed and no version was appended.
async fn add_version_in(
    state: &ApiState,
    workspace: Uuid,
    id: Uuid,
    author: Uuid,
    mut input: NewVersion,
) -> Result<(StatusCode, SkillVersion), ApiError> {
    let files = match input.files.take() {
        Some(f) => Some(store_files(state, workspace, f).await?),
        None => None,
    };
    let (version, appended) = state
        .skills
        .add_version(workspace, id, author, input, files.as_deref())
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
) -> Result<Json<Vec<Skill>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    Ok(Json(state.skills.list(claims.workspace_id).await?))
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
) -> Result<Json<Vec<SkillVersion>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    Ok(Json(state.skills.versions(claims.workspace_id, id).await?))
}

pub async fn get_version(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<SkillVersion>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    Ok(Json(
        state
            .skills
            .version(claims.workspace_id, id, version_id)
            .await?,
    ))
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
                .write(&blob_key(claims.workspace_id, &f.sha256), 0, &bytes)
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

// The operator's own skills ----------------------------------------------------
//
// Reads need no platform route: a workspace already sees the operator's skills
// beside its own, since it cannot decide whether to override what it cannot
// see. Writes do, because they land in the platform workspace rather than the
// caller's, and only the operator may put anything there.

/// The operator writes at the platform level, and nobody else does. Gated the
/// way platform settings are: a system administrator by role, not an authority
/// a workspace could be granted.
fn as_operator(claims: &crate::auth::SessionClaims) -> Result<Uuid, ApiError> {
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
