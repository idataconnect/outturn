mod postgres;

pub use postgres::PostgresSkillStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a skill is: prose in its own right, or instructions layered over one.
///
/// An override is not bound to an agent and never stands alone. It belongs to
/// the workspace and applies wherever its base is used, the same way a setting
/// override applies to every agent that has not spoken for itself -- so a
/// workspace states its variation once rather than per agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    Standalone,
    Override,
}

impl SkillKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillKind::Standalone => "standalone",
            SkillKind::Override => "override",
        }
    }

    fn parse(s: &str) -> SkillKind {
        match s {
            "override" => SkillKind::Override,
            _ => SkillKind::Standalone,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Skill {
    pub id: Uuid,
    /// The platform workspace for a skill the operator ships to everyone, this
    /// workspace's own id otherwise. A caller may read both and write only the
    /// second.
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub kind: SkillKind,
    pub base_skill_id: Option<Uuid>,
    pub forked_from_skill_id: Option<Uuid>,
    pub forked_from_version_id: Option<Uuid>,
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The live version, which is the newest. Absent only for the moment
    /// between a skill being created and its first version landing.
    pub version_id: Option<Uuid>,
    pub ordinal: Option<i32>,
    /// An override whose base has been edited since these instructions were
    /// written against it.
    ///
    /// Nothing breaks when this is true -- which is the problem, and why it is
    /// reported. Instructions that corrected a passage the operator has since
    /// rewritten still compose, still reach the model, and quietly say
    /// something about prose that is no longer there.
    pub base_moved: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillVersion {
    pub id: Uuid,
    pub skill_id: Uuid,
    pub ordinal: i32,
    pub body: String,
    pub note: String,
    pub based_on_version_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkill {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub body: String,
    /// Set to write an override of another skill rather than a skill of one's
    /// own. The base may belong to the operator or to this workspace.
    #[serde(default)]
    pub base_skill_id: Option<Uuid>,
}

/// Fields omitted are left unchanged. The body is not here: prose changes by
/// appending a version, never by overwriting one.
#[derive(Debug, Deserialize)]
pub struct UpdateSkill {
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewVersion {
    pub body: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct ForkSkill {
    pub slug: String,
    pub name: String,
    /// Which version to take. Omitted takes the live one.
    #[serde(default)]
    pub version_id: Option<Uuid>,
}

/// Which skills an agent is given, and in what order they compose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub skill_id: Uuid,
    /// Null follows the skill as it is edited, which is what lets an operator
    /// ship a correction to everyone at once. Set pins this agent to one
    /// version, for a workspace that wants changes reviewed before they arrive.
    #[serde(default)]
    pub version_id: Option<Uuid>,
    #[serde(default)]
    pub position: i32,
}

/// One piece of prose as a turn will actually receive it.
#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub skill_id: Uuid,
    pub version_id: Uuid,
    pub name: String,
    pub kind: SkillKind,
    pub body: String,
    pub position: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("skill not found")]
    NotFound,
    #[error("slug already in use: {0}")]
    DuplicateSlug(String),
    #[error("invalid skill: {0}")]
    Invalid(String),
    #[error("skill store error: {0}")]
    Internal(String),
}

/// Every method takes the workspace explicitly, as the other stores do.
///
/// Reads admit the operator's skills beside the workspace's own, because a
/// workspace cannot decide whether to override something it cannot see. Writes
/// do not: they match on the workspace's own id, so an attempt to edit the
/// operator's skill finds nothing rather than being refused, and overriding or
/// forking stays the only way to vary one.
#[async_trait]
pub trait SkillStore: Send + Sync {
    async fn list(&self, workspace_id: Uuid) -> Result<Vec<Skill>, SkillError>;
    async fn get(&self, workspace_id: Uuid, id: Uuid) -> Result<Skill, SkillError>;
    async fn create(
        &self,
        workspace_id: Uuid,
        author: Uuid,
        input: CreateSkill,
    ) -> Result<Skill, SkillError>;
    async fn update(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        input: UpdateSkill,
    ) -> Result<Skill, SkillError>;
    /// Appends a version, which is how prose changes and how a rollback is
    /// recorded: the caller sends the old body forward rather than moving
    /// anything back.
    async fn add_version(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        author: Uuid,
        input: NewVersion,
    ) -> Result<SkillVersion, SkillError>;
    async fn versions(&self, workspace_id: Uuid, id: Uuid) -> Result<Vec<SkillVersion>, SkillError>;
    async fn version(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        version_id: Uuid,
    ) -> Result<SkillVersion, SkillError>;
    /// Copies a version's body into a skill of this workspace's own, keeping
    /// only a record of where it came from.
    async fn fork(
        &self,
        workspace_id: Uuid,
        source_id: Uuid,
        author: Uuid,
        input: ForkSkill,
    ) -> Result<Skill, SkillError>;
    /// Withdraws a skill without deleting it, so bindings that exist keep
    /// working and the record of what ran stays whole.
    async fn retire(&self, workspace_id: Uuid, id: Uuid, retired: bool)
    -> Result<Skill, SkillError>;
    async fn delete(&self, workspace_id: Uuid, id: Uuid) -> Result<(), SkillError>;

    async fn bindings(&self, workspace_id: Uuid, agent_id: Uuid) -> Result<Vec<Binding>, SkillError>;
    /// Replaces the whole list, because that is what the editor edits.
    async fn set_bindings(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
        bindings: &[Binding],
    ) -> Result<(), SkillError>;

    /// What this agent's next turn should be given, in composition order.
    ///
    /// Each bound skill is followed by this workspace's override of it, if it
    /// has written one. Overrides are not bound and cannot be: they are the
    /// workspace's standing variation, and pulling them in here is what keeps
    /// "we always do it this way" from having to be repeated on every agent.
    async fn resolve_for_agent(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Vec<ResolvedSkill>, SkillError>;

    /// Records what a reply was composed from, which is what an eval reads and
    /// what an audit asks for.
    async fn record_turn(
        &self,
        reply_id: Uuid,
        skills: &[ResolvedSkill],
    ) -> Result<(), SkillError>;
}

pub(super) fn validate_slug(slug: &str) -> Result<(), SkillError> {
    if slug.trim().is_empty() {
        return Err(SkillError::Invalid("slug must not be empty".into()));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(SkillError::Invalid(
            "slug may contain only lowercase letters, digits and hyphens".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_name(name: &str) -> Result<(), SkillError> {
    if name.trim().is_empty() {
        return Err(SkillError::Invalid("name must not be empty".into()));
    }
    Ok(())
}
