//! A generated skill as a derivation: the specification it came from, what
//! people added to it, and regenerating it from both. See
//! docs/openapi-wizard.md, "A derivation, not an output".
//!
//! Turns never read any of this. They read published versions, as they always
//! have; what is here is where people work, and publishing is the moment it
//! becomes something an agent is given.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::Authority;

use super::files::{storage, storage_failed};
use super::router::{ApiError, ApiState, authorize};
use super::skill::wizard::{Annotation, AnnotationKind, Target};
use super::skill::{FileChange, NewFile, NewVersion, SkillFile, VersionSummary, blob_key};

fn internal(e: sqlx::Error) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// An annotation as stored and shown.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnnotationRow {
    #[serde(default)]
    pub id: Uuid,
    /// `skill`, `category` or `operation`.
    pub level: String,
    /// The category's tag or the operation's name. Absent at the skill level.
    #[serde(default)]
    pub target: Option<String>,
    /// `note`, `prefer`, `hidden` or `approval`.
    pub kind: String,
    /// The note, the preferred operation, or the approval rule's YAML.
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AnnotationRow {
    fn read(row: &sqlx::postgres::PgRow) -> Self {
        Self {
            id: row.get("id"),
            level: row.get("level"),
            target: row.get("target"),
            kind: row.get("kind"),
            value: row.get("value"),
            created_at: Some(row.get("created_at")),
        }
    }

    /// The generator's form of it. Checked by `validate` before it is stored,
    /// so a row that does not convert is one written some other way.
    fn annotation(&self) -> Option<Annotation> {
        let target = match (self.level.as_str(), &self.target) {
            ("skill", None) => Target::Skill,
            ("category", Some(tag)) => Target::Category(tag.clone()),
            ("operation", Some(op)) => Target::Operation(op.clone()),
            _ => return None,
        };
        let kind = match self.kind.as_str() {
            "note" => AnnotationKind::Note(self.value.clone()),
            "prefer" => AnnotationKind::Prefer(self.value.clone()),
            "hidden" => AnnotationKind::Hidden,
            "approval" => AnnotationKind::Approval(self.value.clone()),
            _ => return None,
        };
        Some(Annotation { target, kind })
    }

    /// Refused while somebody is looking, for the reasons each one would
    /// otherwise be half-applied.
    fn validate(&self) -> Result<(), String> {
        match (self.level.as_str(), self.target.as_deref()) {
            ("skill", None) => {}
            ("skill", Some(_)) => return Err("a skill-level note names no target".into()),
            ("category" | "operation", Some(t)) if !t.trim().is_empty() => {}
            ("category" | "operation", _) => {
                return Err(format!("a {} annotation names its target", self.level));
            }
            (other, _) => return Err(format!("no level {other}: skill, category or operation")),
        }
        match self.kind.as_str() {
            "note" if self.value.trim().is_empty() => Err("a note says something".into()),
            "note" if self.value.len() > 2000 => Err(
                "a note is at most 2,000 bytes: it is paid for on every turn that reads it".into(),
            ),
            "note" => Ok(()),
            _ if self.level != "operation" => Err(format!("{} is about one operation", self.kind)),
            "prefer" if self.value.trim().is_empty() => {
                Err("a preference names the operation to use instead".into())
            }
            "prefer" | "hidden" => Ok(()),
            "approval" => {
                // Through the same reader a hand-written file goes through at
                // publish, so a rule refused there is refused here, now.
                let file = format!("---\n{}\n---\n# check\n", self.value.trim());
                match super::skill::parse(&file) {
                    Ok(parsed) if parsed.approval.is_some() => Ok(()),
                    Ok(_) => Err("an approval annotation is an `approval:` block".into()),
                    Err(e) => Err(format!("that approval rule is not one: {e}")),
                }
            }
            other => Err(format!("no kind {other}: note, prefer, hidden or approval")),
        }
    }
}

async fn live_annotations(
    pool: &sqlx::PgPool,
    skill_id: Uuid,
) -> Result<Vec<AnnotationRow>, ApiError> {
    Ok(sqlx::query(
        "select id, level, target, kind, value, created_at from skill_annotations \
         where skill_id = $1 and retired_at is null order by id",
    )
    .bind(skill_id)
    .fetch_all(pool)
    .await
    .map_err(internal)?
    .iter()
    .map(AnnotationRow::read)
    .collect())
}

/// Keeps a specification beside the skill made from it: the document in the
/// object store by hash, under the owning workspace, and a row naming it.
async fn store_revision(
    state: &ApiState,
    owner: Uuid,
    skill_id: Uuid,
    actor: Uuid,
    spec: &[u8],
) -> Result<Uuid, ApiError> {
    let sha = hex::encode(Sha256::digest(spec));
    storage(state)?
        .write(&blob_key(owner, &sha), 0, spec)
        .await
        .map_err(storage_failed)?;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into skill_source_revisions (id, skill_id, spec_sha256, spec_bytes, created_by) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(skill_id)
    .bind(&sha)
    .bind(spec.len() as i32)
    .bind(actor)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    Ok(id)
}

/// Records where a skill the wizard just made came from, so it can be made
/// again. Called by the wizard's create.
pub(super) async fn record(
    state: &ApiState,
    owner: Uuid,
    skill_id: Uuid,
    actor: Uuid,
    base_url: &str,
    auth_header: Option<&str>,
    spec: &[u8],
) -> Result<Uuid, ApiError> {
    sqlx::query(
        "insert into skill_sources (skill_id, base_url, auth_header) values ($1, $2, $3) \
         on conflict (skill_id) do update set base_url = $2, auth_header = $3",
    )
    .bind(skill_id)
    .bind(base_url)
    .bind(auth_header)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    store_revision(state, owner, skill_id, actor, spec).await
}

#[derive(serde::Serialize)]
pub struct Source {
    pub base_url: String,
    pub auth_header: Option<String>,
    /// The newest specification kept, if any.
    pub revision: Option<Revision>,
    pub annotations: Vec<AnnotationRow>,
    /// What the newest specification offers to annotate. Absent when none is
    /// kept or it no longer parses.
    pub outline: Option<super::skill::wizard::Outline>,
}

#[derive(serde::Serialize)]
pub struct Revision {
    pub id: Uuid,
    pub spec_sha256: String,
    pub spec_bytes: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

async fn latest_revision(
    pool: &sqlx::PgPool,
    skill_id: Uuid,
) -> Result<Option<Revision>, ApiError> {
    Ok(sqlx::query(
        "select id, spec_sha256, spec_bytes, created_at from skill_source_revisions \
         where skill_id = $1 order by id desc limit 1",
    )
    .bind(skill_id)
    .fetch_optional(pool)
    .await
    .map_err(internal)?
    .map(|r| Revision {
        id: r.get("id"),
        spec_sha256: r.get("spec_sha256"),
        spec_bytes: r.get("spec_bytes"),
        created_at: r.get("created_at"),
    }))
}

/// What a derived skill is made from. `404` for a skill written by hand.
pub async fn get(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Source>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SkillsRead).await?;
    state.skills.get(claims.workspace_id, id).await?;
    let row = sqlx::query("select base_url, auth_header from skill_sources where skill_id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .map_err(internal)?
        .ok_or((
            StatusCode::NOT_FOUND,
            "this skill is not generated from a specification".to_string(),
        ))?;
    let skill = state.skills.get(claims.workspace_id, id).await?;
    let revision = latest_revision(&state.pool, id).await?;
    let outline = match &revision {
        Some(r) => {
            let bytes = storage(&state)?
                .read(
                    &blob_key(skill.workspace_id, &r.spec_sha256),
                    0,
                    r.spec_bytes as u32,
                )
                .await
                .map_err(storage_failed)?;
            super::skill::wizard::outline(&bytes).ok()
        }
        None => None,
    };
    Ok(Json(Source {
        base_url: row.get("base_url"),
        auth_header: row.get("auth_header"),
        revision,
        annotations: live_annotations(&state.pool, id).await?,
        outline,
    }))
}

/// The operator's skill, for a write to it. Derived skills are made by the
/// wizard, which writes the operator's; a workspace's own may follow.
async fn operators(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
    id: Uuid,
) -> Result<(Uuid, crate::api::skill::Skill), ApiError> {
    let claims = super::router::authenticate(state, headers)?;
    let owner = super::skills::as_operator(&claims)?;
    let skill = state.skills.get(owner, id).await?;
    if skill.workspace_id != owner {
        return Err((
            StatusCode::FORBIDDEN,
            "that skill is not the operator's".into(),
        ));
    }
    Ok((claims.subject, skill))
}

pub async fn annotate(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<AnnotationRow>,
) -> Result<(StatusCode, Json<AnnotationRow>), ApiError> {
    let (actor, _) = operators(&state, &headers, id).await?;
    input.validate().map_err(|m| (StatusCode::BAD_REQUEST, m))?;
    let row = sqlx::query(
        "insert into skill_annotations (id, skill_id, level, target, kind, value, created_by) \
         values ($1, $2, $3, $4, $5, $6, $7) \
         returning id, level, target, kind, value, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(&input.level)
    .bind(input.target.as_deref().map(str::trim))
    .bind(&input.kind)
    .bind(input.value.trim())
    .bind(actor)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(AnnotationRow::read(&row))))
}

pub async fn retire(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((id, annotation)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    operators(&state, &headers, id).await?;
    let done = sqlx::query(
        "update skill_annotations set retired_at = now() \
         where id = $1 and skill_id = $2 and retired_at is null",
    )
    .bind(annotation)
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    if done.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "no such annotation".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub struct Regenerate {
    /// A new specification, kept as the next revision. Absent regenerates from
    /// the newest one kept.
    #[serde(default)]
    pub spec: Option<serde_json::Value>,
    /// Needed only for a skill with no source yet -- one made before sources
    /// were kept -- and otherwise changes the stored one.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub auth_header: Option<String>,
    /// Publish what was generated. Without it, the answer is what publishing
    /// would change, and nothing is written but a new specification.
    #[serde(default)]
    pub publish: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(serde::Serialize)]
pub struct Regenerated {
    pub revision: Uuid,
    pub body_changed: bool,
    /// Files added, removed and changed against the live version.
    pub changed: Vec<FileChange>,
    /// Annotations whose category or operation this specification does not
    /// have. Kept, and shown, never dropped.
    pub unmatched: Vec<AnnotationRow>,
    /// The version published, when `publish` was asked for.
    pub version: Option<VersionSummary>,
}

/// Generates a derived skill again from its specification and annotations,
/// and says what that would change -- or, asked to, publishes it.
///
/// A proposal by default, because publishing changes what every bound agent is
/// told and what the gateway refuses: a person reads what changed first.
pub async fn regenerate(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<Regenerate>,
) -> Result<Json<Regenerated>, ApiError> {
    let (actor, skill) = operators(&state, &headers, id).await?;
    let owner = skill.workspace_id;

    let source = sqlx::query("select base_url, auth_header from skill_sources where skill_id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .map_err(internal)?;
    let (base_url, auth_header): (String, Option<String>) = match (&source, &input.base_url) {
        (_, Some(url)) => (
            url.trim().trim_end_matches('/').to_string(),
            input.auth_header.clone(),
        ),
        (Some(row), None) => (
            row.get("base_url"),
            input.auth_header.clone().or(row.get("auth_header")),
        ),
        (None, None) => {
            return Err((
                StatusCode::CONFLICT,
                "this skill has no specification kept; send one with its base_url".into(),
            ));
        }
    };

    let (revision, spec) = match input.spec {
        Some(spec) => {
            let bytes = super::skills::spec_bytes(spec)?;
            let revision = record(
                &state,
                owner,
                id,
                actor,
                &base_url,
                auth_header.as_deref(),
                &bytes,
            )
            .await?;
            (revision, bytes)
        }
        None => {
            if source.is_none() {
                return Err((
                    StatusCode::CONFLICT,
                    "this skill has no specification kept; send one".into(),
                ));
            }
            let latest = latest_revision(&state.pool, id).await?.ok_or((
                StatusCode::CONFLICT,
                "this skill has no specification kept; send one".to_string(),
            ))?;
            let bytes = storage(&state)?
                .read(
                    &blob_key(owner, &latest.spec_sha256),
                    0,
                    latest.spec_bytes as u32,
                )
                .await
                .map_err(storage_failed)?;
            (latest.id, bytes)
        }
    };

    let rows = live_annotations(&state.pool, id).await?;
    let annotations: Vec<Annotation> = rows.iter().filter_map(AnnotationRow::annotation).collect();
    let output = super::skill::wizard::generate(&super::skill::wizard::WizardInput {
        spec_json: spec,
        slug: skill.slug.clone(),
        base_url,
        auth_header,
        annotations,
    })
    .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let unmatched = output.unmatched.iter().map(|&i| rows[i].clone()).collect();

    // Against the live version, by hash, as the history compares versions.
    let live = match skill.version_id {
        Some(v) => Some(state.skills.version(owner, id, v).await?),
        None => None,
    };
    let generated: Vec<SkillFile> = output
        .files
        .iter()
        .map(|(path, content)| SkillFile {
            path: path.clone(),
            sha256: hex::encode(Sha256::digest(content.as_bytes())),
            bytes: content.len() as i32,
            links: None,
        })
        .collect();
    let changed = super::skill::changes(
        live.as_ref()
            .map(|v| v.files.as_slice())
            .unwrap_or_default(),
        &generated,
    );
    let body_changed = live.as_ref().is_none_or(|v| v.body != output.body);

    let version = if input.publish {
        let files: Vec<NewFile> = output
            .files
            .into_iter()
            .map(|(path, content)| NewFile { path, content })
            .collect();
        let note = if input.note.trim().is_empty() {
            "regenerated from its specification".to_string()
        } else {
            input.note
        };
        let (_, published) = super::skills::add_version_in(
            &state,
            owner,
            id,
            actor,
            NewVersion {
                body: output.body,
                note,
                hosts: output.hosts,
                files: Some(files),
            },
        )
        .await?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        sqlx::query(
            "update skill_versions set source_revision_id = $2, annotation_ids = $3 where id = $1",
        )
        .bind(published.id)
        .bind(revision)
        .bind(&ids)
        .execute(&state.pool)
        .await
        .map_err(internal)?;
        tracing::info!(actor = %actor, skill_id = %id, ordinal = published.ordinal, "derived skill regenerated and published");
        Some(VersionSummary::of(&published, live.as_ref()))
    } else {
        None
    };

    Ok(Json(Regenerated {
        revision,
        body_changed,
        changed,
        unmatched,
        version,
    }))
}
