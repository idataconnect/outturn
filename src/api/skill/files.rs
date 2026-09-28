//! A skill version's files: checked, hashed and keyed before anything is
//! stored.

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{NewFile, SkillError, SkillFile};

/// What `read_object` returns in one call. A file larger than this costs the
/// agent several reads to understand, which is the cost the split exists to
/// avoid, so it is refused rather than stored.
pub const MAX_FILE_BYTES: usize = 32 * 1024;

/// Generous for a manifest and a file per operation, and a bound on what one
/// request can make the API hash and upload.
pub const MAX_FILES: usize = 500;

/// Where a file's content lives: under the workspace that owns the version,
/// by hash, so identical content is stored once per workspace.
pub fn blob_key(workspace_id: Uuid, sha256: &str) -> String {
    crate::runtime::storage::scope::skill_blob_key(workspace_id, sha256)
}

/// What a file's frontmatter declared, beside the file itself.
///
/// Carried out of here rather than re-derived later because this is where the
/// content is: a file's bytes go to the object store keyed by hash, and reading
/// them back to parse a declaration would be a round trip for something already
/// in hand. See `docs/approvals.md`.
#[derive(Debug, Clone)]
pub struct DeclaredGate {
    pub path: String,
    pub requires: String,
    /// Upper case, as a request's method is normalised to.
    pub method: String,
    pub path_pattern: String,
    pub identified_by: Option<String>,
    /// The request fields a grant under this gate is keyed on, in declared
    /// order. Never empty: the frontmatter refuses a declaration without them.
    pub binds: Vec<String>,
}

/// Every approval a set of files declares.
pub fn declared_gates(files: &[(SkillFile, Vec<u8>)]) -> Vec<DeclaredGate> {
    let mut out = Vec::new();
    for (file, bytes) in files {
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        // Already validated by `prepare`, which refuses a malformed declaration
        // at publish. A failure here would mean a file that got past it.
        let Ok(parsed) = super::frontmatter::parse(text) else {
            continue;
        };
        let Some(rule) = parsed.approval else {
            continue;
        };
        let Some((method, pattern)) = rule.matches.split_once(' ') else {
            continue;
        };
        out.push(DeclaredGate {
            path: file.path.clone(),
            requires: rule.requires,
            method: method.trim().to_ascii_uppercase(),
            path_pattern: pattern.trim().to_string(),
            identified_by: rule.identified_by,
            binds: rule.binds,
        });
    }
    out
}

/// Checks the set and hashes each file, returning what the store records
/// beside the bytes to upload, sorted by path.
pub fn prepare(files: Vec<NewFile>) -> Result<Vec<(SkillFile, Vec<u8>)>, SkillError> {
    if files.len() > MAX_FILES {
        return Err(SkillError::Invalid(format!(
            "a version may carry at most {MAX_FILES} files"
        )));
    }
    let mut out: Vec<(SkillFile, Vec<u8>)> = Vec::with_capacity(files.len());
    for file in files {
        check_path(&file.path)?;
        let bytes = file.content.into_bytes();
        if bytes.len() > MAX_FILE_BYTES {
            return Err(SkillError::Invalid(format!(
                "{} is {} bytes; a file may be at most {MAX_FILE_BYTES}, what an agent reads in one call",
                file.path,
                bytes.len()
            )));
        }
        // Parsed here, where the content is in hand and a person is waiting for
        // an answer, so a malformed rule is refused at publish rather than
        // discovered when a turn reads the file. `docs/approvals.md` lists what
        // is refused and why each refusal is a rule that would otherwise be
        // half-applied -- and a version is immutable, so a bad declaration
        // published is one somebody has to publish over.
        if let Ok(text) = std::str::from_utf8(&bytes)
            && let Err(e) = super::frontmatter::parse(text)
        {
            return Err(SkillError::Invalid(format!("{}: {e}", file.path)));
        }

        let sha256 = hex::encode(Sha256::digest(&bytes));
        out.push((
            SkillFile {
                path: file.path,
                sha256,
                bytes: bytes.len() as i32,
            },
            bytes,
        ));
    }
    out.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    if let Some(w) = out.windows(2).find(|w| w[0].0.path == w[1].0.path) {
        return Err(SkillError::Invalid(format!(
            "{} is given twice",
            w[0].0.path
        )));
    }
    Ok(out)
}

/// A relative path of plain segments. Refused rather than cleaned up: a path
/// quietly rewritten is one the manifest names differently from the file.
fn check_path(path: &str) -> Result<(), SkillError> {
    let bad = |why: &str| Err(SkillError::Invalid(format!("file path {path:?} {why}")));
    if path.is_empty() || path.len() > 256 {
        return bad("must be 1 to 256 bytes");
    }
    if path.starts_with('/') {
        return bad("must be relative");
    }
    if path.chars().any(|c| c.is_control() || c == '\\') {
        return bad("contains a control character or a backslash");
    }
    if path
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return bad("has an empty, `.` or `..` segment");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &str) -> NewFile {
        NewFile {
            path: path.into(),
            content: content.into(),
        }
    }

    #[test]
    fn hashes_and_sorts() {
        let out = prepare(vec![file("b.md", "two"), file("a.md", "one")]).unwrap();
        let paths: Vec<_> = out.iter().map(|(f, _)| f.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "b.md"]);
        assert_eq!(out[0].0.bytes, 3);
        assert_eq!(out[0].0.sha256, hex::encode(Sha256::digest(b"one")));
    }

    #[test]
    fn refuses_paths_that_climb_or_are_absolute() {
        for p in [
            "",
            "/a.md",
            "../a.md",
            "a/../b.md",
            "a//b.md",
            "./a.md",
            "a\\b.md",
        ] {
            assert!(prepare(vec![file(p, "x")]).is_err(), "{p:?} was accepted");
        }
        assert!(prepare(vec![file("ops/create_booking.md", "x")]).is_ok());
    }

    #[test]
    fn refuses_a_duplicate_path() {
        assert!(prepare(vec![file("a.md", "1"), file("a.md", "2")]).is_err());
    }

    #[test]
    fn refuses_a_file_too_large_to_read_at_once() {
        let big = "x".repeat(MAX_FILE_BYTES + 1);
        assert!(prepare(vec![file("a.md", &big)]).is_err());
        let fits = "x".repeat(MAX_FILE_BYTES);
        assert!(prepare(vec![file("a.md", &fits)]).is_ok());
    }
}

#[cfg(test)]
mod declarations {
    use super::*;

    fn file(path: &str, content: &str) -> NewFile {
        NewFile {
            path: path.into(),
            content: content.into(),
        }
    }

    /// A malformed declaration is refused where somebody is waiting for an
    /// answer, rather than discovered when a turn reads the file.
    ///
    /// A version is immutable, so a bad rule published is one somebody has to
    /// publish over -- and until they do, the file says an operation is gated and
    /// the platform cannot gate it.
    #[test]
    fn a_rule_the_gateway_could_not_apply_is_refused_at_publish() {
        // `covers` with nothing to identify the unit by.
        let err = prepare(vec![file(
            "charge.md",
            "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n  covers: booking\n---\n",
        )])
        .unwrap_err();
        assert!(matches!(err, SkillError::Invalid(_)), "{err:?}");

        // No `matches`, so nothing to gate on.
        let err = prepare(vec![file(
            "charge.md",
            "---\napproval:\n  requires: charge\n---\n",
        )])
        .unwrap_err();
        assert!(matches!(err, SkillError::Invalid(_)), "{err:?}");

        // A star in the middle, which would gate more than it names.
        let err = prepare(vec![file(
            "charge.md",
            "---\napproval:\n  requires: charge\n  matches: POST /char*ges\n---\n",
        )])
        .unwrap_err();
        assert!(matches!(err, SkillError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn the_message_names_the_file() {
        // A publish of forty files should say which one is wrong.
        let err = prepare(vec![
            file("list.md", "# list_rooms\n"),
            file("charge.md", "---\napproval:\n  requires: charge\n---\n"),
        ])
        .unwrap_err();
        let SkillError::Invalid(message) = err else {
            panic!("expected Invalid");
        };
        assert!(message.starts_with("charge.md:"), "{message}");
    }

    #[test]
    fn a_good_declaration_is_read_off_the_file() {
        let prepared = prepare(vec![file(
            "charge.md",
            "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n  \
             covers: booking\n  identified_by: booking_id\n---\n# charge\n",
        )])
        .expect("prepare");
        let gates = declared_gates(&prepared);
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].requires, "charge");
        assert_eq!(gates[0].method, "POST");
        assert_eq!(gates[0].path_pattern, "/charges");
        assert_eq!(gates[0].identified_by.as_deref(), Some("booking_id"));
        assert_eq!(gates[0].path, "charge.md");
    }

    #[test]
    fn a_file_with_no_declaration_contributes_no_gate() {
        let prepared =
            prepare(vec![file("list.md", "# list_rooms\n\nEvery room.\n")]).expect("prepare");
        assert!(declared_gates(&prepared).is_empty());
    }

    #[test]
    fn a_lower_case_method_is_stored_as_the_gateway_matches_it() {
        let prepared = prepare(vec![file(
            "charge.md",
            "---\napproval:\n  requires: charge\n  matches: post /charges\n  binds: [amount_pence]\n---\n",
        )])
        .expect("prepare");
        assert_eq!(declared_gates(&prepared)[0].method, "POST");
    }
}
