mod files;
mod postgres;

pub use files::{MAX_FILE_BYTES, MAX_FILES, blob_key, prepare};

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
    /// What the live version says it will reach.
    pub hosts: Vec<String>,
    /// The declared hosts this workspace has not allowed.
    ///
    /// Computed against the caller's own egress rules, so the operator's skill
    /// reads as unmet to a workspace that has not opened those hosts even
    /// though the operator has. Empty means the skill is ready to bind: every
    /// host it names is one the workspace already permits, whether somebody
    /// allowed it by hand or approved it here.
    pub unmet_hosts: Vec<String>,
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
    pub hosts: Vec<String>,
    pub files: Vec<SkillFile>,
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
    /// Hosts this skill will reach. Names them; opens nothing.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Files the first version carries. Stored by the handler, which passes
    /// the store what it wrote; the store never sees content.
    #[serde(default)]
    pub files: Vec<NewFile>,
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
    /// The hosts this version reaches. Sent in full rather than as a change,
    /// so a version that drops one says so by leaving it out.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// The files this version carries, in full. Omitted keeps the previous
    /// version's, so editing the body alone does not strip them.
    #[serde(default)]
    pub files: Option<Vec<NewFile>>,
}

/// A file as sent. Text, because a skill's files are prose an agent reads.
#[derive(Debug, Deserialize)]
pub struct NewFile {
    pub path: String,
    pub content: String,
}

/// A file a version carries. The content is in the object store under
/// `blob_key` of the owning workspace and this hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillFile {
    pub path: String,
    pub sha256: String,
    pub bytes: i32,
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
    /// Bound a skill that reaches hosts this workspace has not allowed.
    ///
    /// Carries the hosts rather than a sentence about them, so a caller can
    /// name them, and so the approval flow has something to act on. When there
    /// is somewhere to send a request for access, this is where it is raised
    /// from -- see `approve_hosts`, which is the same step taken by somebody
    /// who already holds the authority.
    #[error("needs network access to {}", .0.join(", "))]
    HostsNotAllowed(Vec<String>),
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
        files: &[SkillFile],
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
    ///
    /// `files` of `None` carries the previous version's forward. A version
    /// identical to the live one in body, hosts and files is not appended; the
    /// live one is returned with `false`, so a redeploy grows no history.
    async fn add_version(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        author: Uuid,
        input: NewVersion,
        files: Option<&[SkillFile]>,
    ) -> Result<(SkillVersion, bool), SkillError>;
    async fn versions(&self, workspace_id: Uuid, id: Uuid)
    -> Result<Vec<SkillVersion>, SkillError>;
    async fn version(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        version_id: Uuid,
    ) -> Result<SkillVersion, SkillError>;
    /// Copies a version's body and file list into a skill of this workspace's
    /// own, keeping only a record of where it came from. The caller copies the
    /// file content first when the source belongs to another workspace.
    async fn fork(
        &self,
        workspace_id: Uuid,
        source_id: Uuid,
        author: Uuid,
        input: ForkSkill,
    ) -> Result<Skill, SkillError>;
    /// Withdraws a skill without deleting it, so bindings that exist keep
    /// working and the record of what ran stays whole.
    async fn retire(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        retired: bool,
    ) -> Result<Skill, SkillError>;
    async fn delete(&self, workspace_id: Uuid, id: Uuid) -> Result<(), SkillError>;

    async fn bindings(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Vec<Binding>, SkillError>;
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

    /// Opens the declared hosts this workspace has not allowed yet.
    ///
    /// The caller must hold the authority that writes an egress rule: this is
    /// the ordinary rule-writing act, done in one step and tagged with the
    /// skill that asked for it. Returns what it opened, which is what an
    /// approver should be shown afterwards -- and, before approving, is
    /// exactly `unmet_hosts`, so nobody is asked to re-approve a host they
    /// already allowed.
    async fn approve_hosts(
        &self,
        workspace_id: Uuid,
        skill_id: Uuid,
        actor: Uuid,
    ) -> Result<Vec<String>, SkillError>;

    /// Records what a reply was composed from, which is what an eval reads and
    /// what an audit asks for.
    async fn record_turn(&self, reply_id: Uuid, skills: &[ResolvedSkill])
    -> Result<(), SkillError>;
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

/// The prose a turn is actually given: the agent's own prompt, then each skill
/// it was bound, then whatever this workspace had to say about them.
///
/// Order is the mechanism. A model reads a later instruction as the one that
/// still stands, so an override earns its precedence by being composed after
/// the prose it speaks about rather than by anything in the data saying so.
/// That is a soft guarantee and worth being honest about: it holds well for a
/// direct contradiction and less well for a subtle one, which is why the text
/// that actually ran is recorded rather than inferred from the bindings.
///
/// An agent with no skills is given exactly what it was before, with no
/// heading and no preamble: prose that says "here are your skills" above an
/// empty list is a worse prompt than silence.
/// What the platform tells every agent about itself, before anything a
/// workspace wrote.
///
/// Agents were asked what they are and made something up: one said it was
/// Claude, another denied being the model that was demonstrably serving it and
/// named a product that does not exist, describing its file scopes and tools
/// accurately around the invented name. None of that was a workspace's prompt
/// going wrong -- nothing ever told the agent what it was, so the likeliest
/// continuation was the answer, and a model with no ground truth writes fluent
/// nonsense in whichever direction the conversation leans. It also agrees when
/// challenged, so being corrected does not fix it.
///
/// So the facts are supplied rather than demanded. "Say you do not know" is
/// advice a model cannot follow about something it has no access to; the
/// product, the version and the model actually serving the turn are all things
/// this tier knows, and stating them costs a few tokens once per turn.
///
/// The rule that follows is deliberately narrow. A broad instruction to ground
/// every claim would be a different feature with a cost on every answer, and
/// an agent with no egress rules and no search skill cannot ground anything --
/// it would be told to refuse most of what it is asked. What is ruled out here
/// is inventing specifics about itself and this platform, which is the failure
/// that actually happened.
fn platform_preamble(product: &str, model: &str) -> String {
    format!(
        "You are an agent running on {product} {}, served by the model {model}. \
         Do not invent facts about yourself, this platform, or what you can do: \
         if you have not been told something, say so.",
        env!("CARGO_PKG_VERSION"),
    )
}

/// What this deployment calls itself.
///
/// Configuration rather than source, for the same reason the UI reads
/// `VITE_BRAND_NAME`: this is Apache-2.0 and expects to be run by companies
/// under their own name, and a deployer who has to edit Rust to rename the
/// product carries a patch on a hot file forever. A separate variable from the
/// UI's because that one is compiled into the browser bundle at build time and
/// never reaches this tier.
pub fn product_name() -> String {
    std::env::var("OUTTURN_BRAND_NAME").unwrap_or_else(|_| "outturn".into())
}

/// The prose a turn is given, with the platform's own preamble at the front.
///
/// `model` is what will actually serve this turn, resolved by the caller from
/// the agent's policy and the deployment's default -- not what an agent's
/// prompt claims and not what a guest might report about itself.
pub fn compose_for_turn(system_prompt: &str, skills: &[ResolvedSkill], model: &str) -> String {
    compose_as(&product_name(), system_prompt, skills, model)
}

/// As `compose_for_turn`, for a caller that knows what the deployment is
/// called -- which in practice means a test, so the composition can be checked
/// without reaching into process-wide environment state that every other test
/// shares.
pub fn compose_as(
    product: &str,
    system_prompt: &str,
    skills: &[ResolvedSkill],
    model: &str,
) -> String {
    let composed = compose(system_prompt, skills);
    let preamble = platform_preamble(product, model);
    if composed.trim().is_empty() {
        return preamble;
    }
    format!("{preamble}\n\n{composed}")
}

pub fn compose(system_prompt: &str, skills: &[ResolvedSkill]) -> String {
    if skills.is_empty() {
        return system_prompt.to_string();
    }

    let mut out = String::from(system_prompt.trim_end());
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("# Skills");

    for skill in skills {
        out.push_str("\n\n## ");
        out.push_str(skill.name.trim());
        out.push_str("\n\n");
        if skill.kind == SkillKind::Override {
            out.push_str(
                "This workspace's own instructions. Where they conflict with the skill \
                 above, these are the ones to follow.\n\n",
            );
        }
        out.push_str(skill.body.trim());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent asked what it is should find the answer in front of it.
    ///
    /// The failure this exists for: with nothing in the prompt, agents
    /// invented an identity -- one claimed to be Claude, another denied being
    /// the model serving it and named a product that does not exist. The model
    /// named here is the one the caller resolved for this turn, so the answer
    /// is the platform's rather than the model's guess.
    #[test]
    fn a_turn_is_told_what_is_serving_it() {
        let composed = compose_as(
            "outturn",
            "You are a helpful assistant.",
            &[],
            "qwen3.8:27b-mlx",
        );
        assert!(composed.contains("qwen3.8:27b-mlx"), "{composed}");
        assert!(composed.contains("outturn"), "{composed}");
        assert!(composed.contains(env!("CARGO_PKG_VERSION")), "{composed}");
        // And the agent's own prose is still there, after it.
        assert!(
            composed.ends_with("You are a helpful assistant."),
            "{composed}"
        );
    }

    /// The preamble leads; the workspace's prose follows.
    ///
    /// Same reasoning as the skill ordering below: a model reads a later
    /// instruction as the one that still stands, so an agent that wants a
    /// persona can have one without the platform's facts being buried under
    /// it -- and an agent that contradicts them is at least contradicting
    /// something present rather than filling a vacuum.
    #[test]
    fn the_platform_speaks_before_the_workspace_does() {
        let composed = compose_as("outturn", "You are a pirate.", &[], "gemma4");
        let preamble_at = composed
            .find("You are an agent running on")
            .expect("preamble");
        let prompt_at = composed.find("You are a pirate.").expect("prompt");
        assert!(preamble_at < prompt_at, "{composed}");
    }

    /// An agent with no prompt of its own still gets the facts.
    #[test]
    fn an_empty_prompt_is_still_told_what_it_is() {
        let composed = compose_as("outturn", "", &[], "gemma4");
        assert!(composed.contains("gemma4"), "{composed}");
        assert!(
            !composed.starts_with('\n'),
            "no leading blank: {composed:?}"
        );
    }

    /// Skills keep their place, after the preamble and the agent's prose.
    #[test]
    fn the_preamble_does_not_displace_the_skills() {
        let skills = vec![skill(
            "Search",
            "Use the search API.",
            SkillKind::Standalone,
        )];
        let composed = compose_as("outturn", "You are terse.", &skills, "gemma4");
        let prompt_at = composed.find("You are terse.").expect("prompt");
        let skills_at = composed.find("# Skills").expect("skills");
        assert!(prompt_at < skills_at, "{composed}");
        assert!(composed.contains("Use the search API."), "{composed}");
    }

    /// A deployer's own name reaches the agent, not just the browser.
    ///
    /// The UI reads `VITE_BRAND_NAME`, which is compiled into the bundle and
    /// never reaches this tier; without its own variable an agent would tell a
    /// deployer's customers they are talking to outturn.
    #[test]
    fn a_deployment_can_say_what_it_is_called() {
        let composed = compose_as("Hollowbrook", "", &[], "gemma4");
        assert!(composed.contains("Hollowbrook"), "{composed}");
        assert!(!composed.contains("outturn"), "{composed}");
    }

    fn skill(name: &str, body: &str, kind: SkillKind) -> ResolvedSkill {
        ResolvedSkill {
            skill_id: Uuid::nil(),
            version_id: Uuid::nil(),
            name: name.into(),
            kind,
            body: body.into(),
            position: 0,
        }
    }

    /// An agent with nothing bound is given what it always was.
    #[test]
    fn no_skills_leaves_the_prompt_alone() {
        assert_eq!(compose("Be helpful.", &[]), "Be helpful.");
    }

    #[test]
    fn a_skill_follows_the_prompt_under_its_own_heading() {
        let out = compose(
            "Be helpful.",
            &[skill("CRM", "Call v1.", SkillKind::Standalone)],
        );
        assert_eq!(out, "Be helpful.\n\n# Skills\n\n## CRM\n\nCall v1.");
    }

    /// The override lands after the prose it corrects, and says so.
    #[test]
    fn an_override_is_composed_last_and_claims_precedence() {
        let out = compose(
            "Be helpful.",
            &[
                skill("CRM", "Call v1.", SkillKind::Standalone),
                skill("CRM (ours)", "Call v2.", SkillKind::Override),
            ],
        );
        let base = out.find("Call v1.").expect("base");
        let over = out.find("Call v2.").expect("override");
        assert!(base < over, "the override must come after its base:\n{out}");
        assert!(
            out.contains("these are the ones to follow"),
            "the override did not claim precedence:\n{out}"
        );
    }

    /// An agent may have no prompt of its own and still be given skills.
    #[test]
    fn an_empty_prompt_gains_no_leading_blank_lines() {
        let out = compose("", &[skill("CRM", "Call v1.", SkillKind::Standalone)]);
        assert!(out.starts_with("# Skills"), "{out:?}");
    }
}
