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
    format!("skills/{workspace_id}/{sha256}")
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
