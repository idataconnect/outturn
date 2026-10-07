//! Agent templates: an agent an operator defines once and has made in every
//! workspace that should have it. See docs/agent-templates.md.

mod postgres;

pub use postgres::PostgresAgentTemplateStore;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How workspaces come to have a template's agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
    /// Made in every workspace, and not removable there.
    Required,
    /// Made in every workspace; its admin may remove it.
    Default,
    /// Offered in the catalog; made when an admin adds it.
    Optional,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Availability::Required => "required",
            Availability::Default => "default",
            Availability::Optional => "optional",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "required" => Some(Availability::Required),
            "default" => Some(Availability::Default),
            "optional" => Some(Availability::Optional),
            _ => None,
        }
    }

    /// Whether every workspace is given one without asking.
    pub fn provisioned(self) -> bool {
        matches!(self, Availability::Required | Availability::Default)
    }
}

/// A skill a template gives its agents: an operator skill, following or
/// pinned as an agent's own binding is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateSkill {
    pub skill_id: Uuid,
    #[serde(default)]
    pub version_id: Option<Uuid>,
}

/// One publish of a template. Never changed once written.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub ordinal: i32,
    pub name: String,
    pub description: String,
    pub requirements: String,
    pub defaults: String,
    pub reminder: String,
    pub policy: serde_json::Value,
    pub eager_tools: Vec<String>,
    pub skills: Vec<TemplateSkill>,
    /// Settings it fixes, by catalog key. They win over the workspace's and
    /// the agent's own.
    pub settings: serde_json::Map<String, serde_json::Value>,
    pub note: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Template {
    pub id: Uuid,
    pub slug: String,
    pub availability: Availability,
    pub allow_additions: bool,
    /// Whether a workspace may keep its agent on a version rather than follow
    /// the newest.
    pub allow_pinning: bool,
    pub retired: bool,
    /// The newest version, which every agent made from it follows unless its
    /// workspace pinned another.
    pub current: TemplateVersion,
}

/// A template as one agent runs it: the template, and the version this agent
/// is on -- the newest, or the one its workspace pinned.
#[derive(Debug, Clone)]
pub struct AgentTemplate {
    pub template: Template,
    pub running: TemplateVersion,
}

/// What a publish says. The whole of a version, since versions are not edited.
#[derive(Debug, Clone, Deserialize)]
pub struct NewVersion {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub requirements: String,
    #[serde(default)]
    pub defaults: String,
    #[serde(default)]
    pub reminder: String,
    #[serde(default)]
    pub policy: Option<serde_json::Value>,
    #[serde(default)]
    pub eager_tools: Vec<String>,
    #[serde(default)]
    pub skills: Vec<TemplateSkill>,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewTemplate {
    pub slug: String,
    #[serde(default = "optional")]
    pub availability: Availability,
    #[serde(default = "yes")]
    pub allow_additions: bool,
    #[serde(default)]
    pub allow_pinning: bool,
    #[serde(flatten)]
    pub version: NewVersion,
}

fn optional() -> Availability {
    Availability::Optional
}

fn yes() -> bool {
    true
}

/// Fields omitted are left as they are.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTemplate {
    pub availability: Option<Availability>,
    pub allow_additions: Option<bool>,
    pub allow_pinning: Option<bool>,
    pub retired: Option<bool>,
}

/// A template as a workspace's catalog shows it.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub template_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub availability: Availability,
    /// This workspace's agent made from it, if it has one.
    pub agent_id: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("template not found")]
    NotFound,
    #[error("a template with the slug {0} already exists")]
    DuplicateSlug(String),
    #[error("{0}")]
    Invalid(String),
    /// Refused for a reason the caller can act on, said in full.
    #[error("{0}")]
    Refused(String),
    #[error("template store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait AgentTemplateStore: Send + Sync {
    /// Every template, retired ones included, for the operator.
    async fn list(&self) -> Result<Vec<Template>, TemplateError>;
    async fn get(&self, id: Uuid) -> Result<Template, TemplateError>;
    async fn create(
        &self,
        input: NewTemplate,
        created_by: Option<Uuid>,
    ) -> Result<Template, TemplateError>;
    /// Writes a new version, which every agent made from the template follows
    /// from its next turn.
    async fn publish(
        &self,
        id: Uuid,
        input: NewVersion,
        created_by: Option<Uuid>,
    ) -> Result<Template, TemplateError>;
    async fn update(&self, id: Uuid, input: UpdateTemplate) -> Result<Template, TemplateError>;
    async fn versions(&self, id: Uuid) -> Result<Vec<TemplateVersion>, TemplateError>;

    /// The template an agent was made from, and the version it runs: the one
    /// its workspace pinned, while the template allows pinning, else the
    /// newest. None for an agent made by hand.
    async fn for_agent(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Option<AgentTemplate>, TemplateError>;

    /// Keeps a workspace's agent on one version of its template, or with
    /// `None` lets it follow the newest again.
    async fn pin(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
        version_id: Option<Uuid>,
    ) -> Result<(), TemplateError>;

    /// The templates a workspace may see, and which it has.
    async fn catalog(&self, workspace_id: Uuid) -> Result<Vec<CatalogEntry>, TemplateError>;

    /// Makes the agents a workspace is owed -- one for each required template,
    /// and each default one it has not removed -- and brings the names of the
    /// agents it already has up to date. `None` does every workspace. Safe to
    /// run again; returns how many agents were made.
    async fn provision(&self, workspace_id: Option<Uuid>) -> Result<usize, TemplateError>;

    /// Makes a workspace's agent from a template it chose, and forgets that it
    /// once removed it. Returns the agent's id.
    async fn install(&self, workspace_id: Uuid, template_id: Uuid) -> Result<Uuid, TemplateError>;

    /// Asked before a template's agent is deleted: refused for a required
    /// template, and remembered for a default one so it is not made again.
    async fn removing(&self, workspace_id: Uuid, template_id: Uuid) -> Result<(), TemplateError>;
}

/// The size a workspace's section of a prompt may be. A section a business
/// writes about how it works is a few paragraphs; anything longer is trying to
/// be a different agent, and every byte of it is spent on every round.
pub const MAX_ADDITION_BYTES: usize = 4 * 1024;

/// A template agent's system prompt: the operator's requirements and
/// defaults, the workspace's own section after both, and the requirements
/// restated last.
///
/// The order is the point. A model reads its prompt one token at a time, each
/// able to look only at what came before it, so the workspace's text and the
/// line saying how it relates come after both of the operator's parts, where
/// they are read with those in view. The reminder is read last of all.
pub fn compose_prompt(version: &TemplateVersion, addition: Option<&str>) -> String {
    let mut out = String::new();
    let mut section = |heading: &str, body: &str| {
        let body = body.trim();
        if body.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        if !heading.is_empty() {
            out.push_str(heading);
            out.push('\n');
        }
        out.push_str(body);
    };
    section("## Requirements", &version.requirements);
    section("## Defaults", &version.defaults);
    if let Some(addition) = addition.map(str::trim).filter(|a| !a.is_empty()) {
        section(
            "## From this workspace",
            &format!(
                "How this business works. Where it differs from the defaults above, follow \
                 it. Never set aside the requirements.\n\n{addition}"
            ),
        );
    }
    if !version.reminder.trim().is_empty() {
        section(
            "",
            &format!("Requirements still apply: {}", version.reminder.trim()),
        );
    }
    out
}

pub(crate) fn validate_slug(slug: &str) -> Result<(), TemplateError> {
    if slug.is_empty()
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(TemplateError::Invalid(
            "a slug is lowercase letters, digits and hyphens".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(requirements: &str, defaults: &str, reminder: &str) -> TemplateVersion {
        TemplateVersion {
            id: Uuid::nil(),
            template_id: Uuid::nil(),
            ordinal: 1,
            name: "Invoicer".into(),
            description: String::new(),
            requirements: requirements.into(),
            defaults: defaults.into(),
            reminder: reminder.into(),
            policy: serde_json::json!({}),
            eager_tools: vec![],
            skills: vec![],
            settings: Default::default(),
            note: String::new(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn the_workspace_section_comes_after_both_of_the_operators() {
        let v = version(
            "Quote only listed prices.",
            "Raise duplicates.",
            "listed prices only",
        );
        let prompt = compose_prompt(&v, Some("Void duplicates instead of raising them."));
        let at = |s: &str| {
            prompt
                .find(s)
                .unwrap_or_else(|| panic!("{s} missing: {prompt}"))
        };
        assert!(at("## Requirements") < at("## Defaults"));
        assert!(at("## Defaults") < at("## From this workspace"));
        assert!(at("## From this workspace") < at("Requirements still apply"));
        assert!(prompt.ends_with("Requirements still apply: listed prices only"));
    }

    #[test]
    fn empty_parts_leave_no_headings_behind() {
        let v = version("Be accurate.", "", "");
        assert_eq!(
            compose_prompt(&v, Some("  ")),
            "## Requirements\nBe accurate."
        );
    }
}
