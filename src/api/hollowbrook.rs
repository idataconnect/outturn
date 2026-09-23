//! Installs the Hollowbrook skill, so `--with hollowbrook` is one flag.
//!
//! The guesthouse is a fixture with a REST API, and this is what lets an agent
//! use it: a skill saying what the operations are, the detail files saying how
//! to call each one, and the egress rule permitting the host.
//!
//! ## Why the skill is a manifest rather than prose
//!
//! A skill body is composed into the system prompt on every round of every
//! turn (`skill::compose`), so an API rendered faithfully into one is paid for
//! continuously whether or not the agent uses it. docs/openapi-wizard.md is
//! the design this follows: the body is a list of operations, and the detail
//! for each is a file the agent reads with `read_object` when it decides it
//! needs that operation.
//!
//! Hollowbrook is far too small to need the split -- five operations fit in a
//! body at about 5KB -- and it is done anyway, because what is being tried out
//! is the shape. Whether a model that has read a manifest line actually goes
//! and reads the file, rather than guessing the call from the name, is the
//! question the wizard is built on, and five operations is a better place to
//! find out than two hundred.
//!
//! ## Why the files are embedded
//!
//! `include_str!` rather than a directory copied into the image: a missing
//! file is then a build failure rather than a skill whose manifest points at
//! nothing, which an agent would discover by reading a path and being told it
//! does not exist.

use std::sync::Arc;

use uuid::Uuid;

use super::skill::{CreateSkill, SkillStore};
use crate::runtime::storage::{StorageBackend, scope};

/// The host an operator has to have opened, and the one the skill declares.
///
/// The two are different acts. `OUTTURN_INTERNAL_HOSTS` on the gateway says
/// the platform may reach a private address at all; this says the workspace
/// allows its agents to ask. Neither substitutes for the other.
const HOST: &str = "outturn-hollowbrook:8084";

const SLUG: &str = "hollowbrook";

/// The manifest, which becomes the skill body.
const MANIFEST: &str = include_str!("../../assets/skills/hollowbrook/index.md");

/// One file per operation, named as the manifest names it. The path an agent
/// reads is derived from the operation's name, so a model that has read the
/// manifest can construct it rather than having to remember it from
/// elsewhere in the prompt.
const DETAIL: &[(&str, &str)] = &[
    (
        "list_rooms",
        include_str!("../../assets/skills/hollowbrook/list_rooms.md"),
    ),
    (
        "check_availability",
        include_str!("../../assets/skills/hollowbrook/check_availability.md"),
    ),
    (
        "list_bookings",
        include_str!("../../assets/skills/hollowbrook/list_bookings.md"),
    ),
    (
        "get_booking",
        include_str!("../../assets/skills/hollowbrook/get_booking.md"),
    ),
    (
        "create_booking",
        include_str!("../../assets/skills/hollowbrook/create_booking.md"),
    ),
];

/// Installs the skill and its detail files into a workspace.
///
/// Runs when OUTTURN_SEED_HOLLOWBROOK is set, which the component sets and
/// nothing else does -- so a cluster without `--with hollowbrook` never hears
/// of it. Idempotent by the slug: a second run finds the skill and leaves it
/// alone rather than writing a second copy.
pub async fn seed(
    skills: &Arc<dyn SkillStore>,
    storage: Option<&Arc<dyn StorageBackend>>,
    workspace_id: Uuid,
    author: Uuid,
) -> anyhow::Result<()> {
    if std::env::var("OUTTURN_SEED_HOLLOWBROOK").is_err() {
        return Ok(());
    }

    // The files first. A manifest pointing at files that are not there yet is
    // a skill that is wrong for as long as the gap lasts, and an agent reading
    // in that window gets told the path does not exist.
    let Some(store) = storage else {
        anyhow::bail!("OUTTURN_SEED_HOLLOWBROOK is set but there is no object store to write to");
    };
    let space = scope::Space {
        workspace_id,
        // Unused: a workspace-scoped key is `workspaces/{id}/...` and names
        // neither. Filled with nil rather than something plausible, so nothing
        // reads as a real agent or session.
        agent_id: Uuid::nil(),
        session_id: Uuid::nil(),
    };

    for (name, body) in DETAIL {
        let path = format!("workspace/api/hollowbrook/{name}.md");
        let key =
            scope::resolve(&space, &path).map_err(|e| anyhow::anyhow!("resolving {path}: {e}"))?;
        store
            .write(&key, 0, body.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("writing {path}: {e}"))?;
    }

    // The manifest again as a file, so the same listing shows what the prompt
    // says -- a reader looking at `workspace/api/hollowbrook/` finds the index
    // beside the operations rather than only in the prompt.
    let index = scope::resolve(&space, "workspace/api/hollowbrook/index.md")
        .map_err(|e| anyhow::anyhow!("resolving the index: {e}"))?;
    store
        .write(&index, 0, MANIFEST.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("writing the index: {e}"))?;

    // Then the skill. Second, so the prompt never names a file that is not
    // written yet.
    let existing = skills.list(workspace_id).await?;
    if existing.iter().any(|s| s.slug == SLUG) {
        tracing::debug!("hollowbrook skill already installed");
        return Ok(());
    }

    let skill = skills
        .create(
            workspace_id,
            author,
            CreateSkill {
                slug: SLUG.into(),
                name: "Hollowbrook House".into(),
                description: "Rooms and bookings for the guesthouse.".into(),
                body: MANIFEST.into(),
                base_skill_id: None,
                // Declared, which opens nothing on its own: it is what makes
                // the host show as unmet until somebody allows it, and what
                // `approve_hosts` then opens in one step.
                hosts: vec![HOST.into()],
            },
        )
        .await?;

    // Approved here because the operator asking for this component is the
    // person who would otherwise be approving it: a demonstration that comes
    // up needing a host allowed by hand is a demonstration nobody sees work.
    let opened = skills.approve_hosts(workspace_id, skill.id, author).await?;

    tracing::info!(
        skill = %skill.id,
        hosts = ?opened,
        files = DETAIL.len() + 1,
        "installed the Hollowbrook skill"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest names an operation for every file, and a file for every
    /// operation.
    ///
    /// A manifest line pointing at a file that is not written is a path the
    /// agent is told does not exist, and a file nothing points at is prose
    /// nobody will ever read -- both silent, and both the ordinary way a
    /// hand-written pair of things drifts.
    #[test]
    fn every_operation_named_has_a_file_and_the_reverse() {
        for (name, _) in DETAIL {
            assert!(
                MANIFEST.contains(&format!("`{name}`")),
                "{name} has a file that the manifest never names"
            );
            assert!(
                MANIFEST.contains(&format!("workspace/api/hollowbrook/{name}.md")),
                "the manifest names {name} without saying where to read it"
            );
        }

        // And nothing named that is not there. Counted rather than parsed: the
        // manifest is prose, and a parser for it would be a second thing to
        // get wrong.
        let named = MANIFEST.matches("workspace/api/hollowbrook/").count();
        assert_eq!(
            named,
            DETAIL.len(),
            "the manifest points at {named} files and there are {}",
            DETAIL.len()
        );
    }

    /// Every file fits in one `read_object`.
    ///
    /// The split exists to keep the prompt small, and it pays for itself only
    /// if reading one operation costs one call. READ_BUDGET is 32KB; a file
    /// past it is returned in pieces and the agent pays for the split twice.
    #[test]
    fn every_file_is_one_read() {
        const READ_BUDGET: usize = 32 * 1024;
        for (name, body) in DETAIL {
            assert!(
                body.len() < READ_BUDGET,
                "{name} is {} bytes, which is more than one read",
                body.len()
            );
        }
    }

    /// The manifest never shows a URL.
    ///
    /// docs/openapi-wizard.md is explicit about this, and
    /// docs/skill-evaluation.md is where it comes from: a skill that documents
    /// its call the way an API reference does reads correctly to a capable
    /// model and gets called as a tool name by a weaker one. A URL belongs in
    /// the detail file, beside the method and an instruction to use
    /// `fetch_url`, where there is enough around it to be unambiguous.
    #[test]
    fn the_manifest_shows_no_url() {
        assert!(
            !MANIFEST.contains("http://") && !MANIFEST.contains("https://"),
            "the manifest names a URL, which a weaker model will call as a tool"
        );
    }

    /// Every detail file says to use `fetch_url`, and says what host.
    ///
    /// The manifest deliberately does not, so the file is the only place an
    /// agent learns how to make the call. A file that names a method and a
    /// path without naming the tool leaves that to be guessed.
    #[test]
    fn every_detail_file_says_how_to_call_it() {
        for (name, body) in DETAIL {
            assert!(
                body.contains("fetch_url"),
                "{name} does not say to use fetch_url"
            );
            assert!(
                body.contains(HOST),
                "{name} does not name {HOST}, so the agent has nowhere to send it"
            );
        }
    }
}
