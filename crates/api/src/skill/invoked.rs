//! A skill somebody picked from the composer's `/` menu, and the note the
//! model is given about it.
//!
//! The pick is stored on the message as `metadata.skill`, beside what the
//! person typed and never inside it: the content stays their words, and the
//! transcript draws the skill as a mark rather than as a sentence nobody
//! wrote. The note is derived each time the conversation is projected, from
//! the mark and the skills as the turn has them, so a retry and a compacted
//! conversation say the same thing about the same message. See
//! `docs/prompt-contributors.md`.

use std::collections::HashMap;
use std::sync::Arc;

use outturn_core::storage::{StorageBackend, scope};
use uuid::Uuid;

use super::{ResolvedSkill, SkillKind, blob_key};
use crate::chat::Message;

/// Where a user message records the skill it invoked.
pub const MARK: &str = "skill";

/// Where the note derived for a message is held between preparing it and
/// projecting it. Never stored: written into the copy of the history a turn
/// is prepared from, which is the only place the projection reads.
pub const NOTE: &str = "skill_note";

/// How much of a skill's main reference file goes into the note. Enough for a
/// manifest or an index; anything longer is the agent's to read, and the note
/// says where.
pub const REFERENCE_BYTES: usize = 16 * 1024;

/// What a message's mark says.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Invoked {
    pub slug: String,
    /// The name the skill had when it was picked, for drawing the mark. The
    /// note names it as the turn's skills do, since that is the heading the
    /// model can find.
    pub name: String,
}

impl Invoked {
    pub fn of(message: &Message) -> Option<Invoked> {
        if message.role != "user" {
            return None;
        }
        serde_json::from_value(message.metadata.get(MARK)?.clone()).ok()
    }
}

/// The skill an invocation names among those a turn has: the standalone one,
/// since an override shares its base's slug and is not something to invoke.
pub fn find<'a>(skills: &'a [ResolvedSkill], slug: &str) -> Option<&'a ResolvedSkill> {
    skills
        .iter()
        .find(|s| s.kind == SkillKind::Standalone && s.slug == slug)
}

/// The file to put in front of the agent: `index.md` where there is one, or
/// else the file the body names first. None when the body names none, since
/// guessing which of several files matters is the skill author's call.
pub fn main_file(skill: &ResolvedSkill) -> Option<&super::SkillFile> {
    if let Some(index) = skill.files.iter().find(|f| f.path == "index.md") {
        return Some(index);
    }
    skill
        .files
        .iter()
        .filter_map(|f| skill.body.find(f.path.as_str()).map(|at| (at, f)))
        .min_by_key(|(at, _)| *at)
        .map(|(_, f)| f)
}

/// A main file as read: its path and as much of it as fits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

/// What the model is told about an invocation.
///
/// `in_prompt` is whether the skill's body is in the system prompt this turn
/// is sent; when it is not, the body comes with the note instead. `typed`
/// is whether the person wrote anything beside picking it, because a pick
/// alone is the whole request and has to be read as one.
pub fn note(
    skill: &ResolvedSkill,
    in_prompt: bool,
    typed: bool,
    reference: Option<&Reference>,
) -> String {
    let name = skill.name.trim();
    let mut out = if typed {
        format!("The person invoked the skill \"{name}\" with the message above.")
    } else {
        format!("The person invoked the skill \"{name}\" with no further instructions.")
    };
    if in_prompt {
        out.push_str(&format!(
            " Follow that skill's instructions, which are in your system prompt \
             under \"## {name}\"."
        ));
    } else {
        out.push_str(&format!(
            " Follow that skill's instructions, which are these:\n\n{}",
            skill.body.trim()
        ));
    }

    let others: Vec<String> = skill
        .files
        .iter()
        .filter(|f| reference.is_none_or(|r| r.path != f.path))
        .map(|f| format!("- `{}`", scope::skill_object_path(&skill.slug, &f.path)))
        .collect();
    if let Some(r) = reference {
        let at = scope::skill_object_path(&skill.slug, &r.path);
        out.push_str(&format!(
            "\n\nThe skill's main reference file, `{at}`:\n\n{}",
            r.content
        ));
        if r.truncated {
            out.push_str(&format!(
                "\n\n[cut off at {} KB; read the rest of `{at}` with read_object]",
                REFERENCE_BYTES / 1024
            ));
        }
    }
    if !others.is_empty() {
        out.push_str(&format!(
            "\n\n{} reference files, read with read_object when the skill calls for one:\n{}",
            if reference.is_some() {
                "Its other"
            } else {
                "Its"
            },
            others.join("\n")
        ));
    }
    out
}

/// Reads as much of a main file as goes into a note. None when there is no
/// object store or the read fails: the note still lists the file by path, and
/// the agent can read it itself.
pub async fn read_reference(
    storage: Option<&Arc<dyn StorageBackend>>,
    skill: &ResolvedSkill,
    file: &super::SkillFile,
) -> Option<Reference> {
    let store = storage?;
    let len = (file.bytes.max(0) as usize).min(REFERENCE_BYTES + 4);
    let bytes = match store
        .read(&blob_key(skill.owner, &file.sha256), 0, len as u32)
        .await
    {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(skill = %skill.slug, path = %file.path, error = %e,
                "could not read an invoked skill's reference file; naming it instead");
            return None;
        }
    };
    let truncated = file.bytes as usize > REFERENCE_BYTES;
    let mut cut = bytes.len().min(REFERENCE_BYTES);
    // Back to a character boundary, so a cut never lands inside one.
    while cut > 0 && std::str::from_utf8(&bytes[..cut]).is_err() {
        cut -= 1;
    }
    Some(Reference {
        path: file.path.clone(),
        content: String::from_utf8_lossy(&bytes[..cut]).into_owned(),
        truncated,
    })
}

/// Writes each invoking message's note into the history a turn projects.
///
/// `live` is what an invocation is looked up in, and what the body is checked
/// against together with `prompt`, the skills the system prompt being sent was
/// composed from: a body present in only one of them may not be in what the
/// turn is sent once compaction has chosen, so it comes with the note. An
/// invocation of a skill the agent no longer has is left as the person's text
/// alone -- there is nothing to point at.
pub async fn prepare(
    history: &mut [Message],
    live: &[ResolvedSkill],
    prompt: &[ResolvedSkill],
    storage: Option<&Arc<dyn StorageBackend>>,
) {
    // Read once per skill, however often it was invoked.
    let mut read: HashMap<Uuid, Option<Reference>> = HashMap::new();
    for message in history.iter_mut() {
        let Some(invoked) = Invoked::of(message) else {
            continue;
        };
        let Some(skill) = find(live, &invoked.slug) else {
            continue;
        };
        let reference = match read.get(&skill.version_id) {
            Some(r) => r.clone(),
            None => {
                let r = match main_file(skill) {
                    Some(file) => read_reference(storage, skill, file).await,
                    None => None,
                };
                read.insert(skill.version_id, r.clone());
                r
            }
        };
        let in_prompt = find(prompt, &invoked.slug).is_some();
        let text = note(
            skill,
            in_prompt,
            !message.content.trim().is_empty(),
            reference.as_ref(),
        );
        if let Some(meta) = message.metadata.as_object_mut() {
            meta.insert(NOTE.into(), serde_json::Value::String(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> super::super::SkillFile {
        super::super::SkillFile {
            path: path.into(),
            sha256: "0".repeat(64),
            bytes: 10,
            links: None,
            operation: None,
        }
    }

    fn skill(body: &str, files: Vec<super::super::SkillFile>) -> ResolvedSkill {
        ResolvedSkill {
            skill_id: Uuid::now_v7(),
            version_id: Uuid::now_v7(),
            slug: "booking".into(),
            owner: Uuid::now_v7(),
            name: "Booking".into(),
            kind: SkillKind::Standalone,
            body: body.into(),
            files,
            position: 0,
        }
    }

    #[test]
    fn index_is_the_main_file_and_otherwise_the_first_the_body_names() {
        let s = skill("Read b.md, then a.md.", vec![file("a.md"), file("b.md")]);
        assert_eq!(main_file(&s).map(|f| f.path.as_str()), Some("b.md"));
        let s = skill("Read b.md.", vec![file("b.md"), file("index.md")]);
        assert_eq!(main_file(&s).map(|f| f.path.as_str()), Some("index.md"));
        let s = skill("Nothing named.", vec![file("a.md")]);
        assert!(main_file(&s).is_none());
    }

    #[test]
    fn a_body_in_the_prompt_is_pointed_at_rather_than_repeated() {
        let s = skill("Book rooms carefully.", vec![]);
        let pointed = note(&s, true, true, None);
        assert!(pointed.contains("## Booking"), "{pointed}");
        assert!(!pointed.contains("Book rooms carefully."), "{pointed}");
        let carried = note(&s, false, true, None);
        assert!(carried.contains("Book rooms carefully."), "{carried}");
    }

    #[test]
    fn a_pick_alone_is_said_to_be_the_whole_request() {
        let s = skill("x", vec![]);
        assert!(note(&s, true, false, None).contains("with no further instructions"));
        assert!(note(&s, true, true, None).contains("with the message above"));
    }

    #[test]
    fn the_main_file_is_included_and_the_rest_listed() {
        let s = skill("x", vec![file("index.md"), file("rates.md")]);
        let r = Reference {
            path: "index.md".into(),
            content: "Rates are in rates.md.".into(),
            truncated: true,
        };
        let n = note(&s, true, true, Some(&r));
        assert!(n.contains("Rates are in rates.md."), "{n}");
        assert!(n.contains("cut off at 16 KB"), "{n}");
        let rates = scope::skill_object_path("booking", "rates.md");
        assert!(n.contains(&format!("- `{rates}`")), "{n}");
        // The included file is not listed a second time.
        let index = scope::skill_object_path("booking", "index.md");
        assert!(!n.contains(&format!("- `{index}`")), "{n}");
    }
}
