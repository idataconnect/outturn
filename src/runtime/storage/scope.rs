//! Mapping what a guest asks for onto where it actually lives.
//!
//! A guest names `session/notes.md` or `workspace/reference/pricing.csv` and never
//! learns which workspace, agent or session it is. That is the point: a component
//! cannot get a workspace wrong if it is never told one, and cannot reach another's
//! data by constructing a path, because every path is resolved by the host
//! against the space this turn was given.
//!
//! The scope is the first segment of the path, not a separate argument. A path
//! is the one thing every model reliably produces, and one extra segment is
//! far less confusing than a second parameter it has to remember to pair with
//! the first. It also makes "list everything" meaningful: the three scopes
//! are the three folders.
//!
//! Scopes are lifetimes, laid out scope-first so an S3 lifecycle rule can
//! match each with one prefix (see docs/storage.md for why the obvious
//! hierarchy cannot carry retention):
//!
//! ```text
//! workspace/...   ->  workspaces/<workspace>/...                       kept until deleted
//! agent/...    ->  agents/<workspace>/<agent>/...                kept while the agent exists
//! session/...  ->  sessions/<workspace>/<agent>/<session>/...    swept
//! ```

use uuid::Uuid;

use super::StorageError;

/// Where a turn's files live: the three ids every scope is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub session_id: Uuid,
}

/// How long a file lives, which is what a guest chooses when it picks a
/// prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Workspace,
    Agent,
    Session,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Session, Scope::Agent, Scope::Workspace];

    /// The segment a guest writes.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Workspace => "workspace",
            Scope::Agent => "agent",
            Scope::Session => "session",
        }
    }

    pub fn parse(segment: &str) -> Option<Scope> {
        match segment {
            "workspace" => Some(Scope::Workspace),
            "agent" => Some(Scope::Agent),
            "session" => Some(Scope::Session),
            _ => None,
        }
    }

    /// The bucket prefix everything in this scope, across all workspaces, sits
    /// under. One lifecycle rule per scope matches this.
    pub fn bucket_prefix(self) -> &'static str {
        match self {
            Scope::Workspace => "workspaces/",
            Scope::Agent => "agents/",
            Scope::Session => "sessions/",
        }
    }
}

/// The real prefix of one scope of one space.
pub fn root_for(space: &Space, scope: Scope) -> String {
    match scope {
        Scope::Workspace => format!("workspaces/{}/", space.workspace_id),
        Scope::Agent => format!("agents/{}/{}/", space.workspace_id, space.agent_id),
        Scope::Session => format!(
            "sessions/{}/{}/{}/",
            space.workspace_id, space.agent_id, space.session_id
        ),
    }
}

/// What a guest is told when its path names no scope, or one that does not
/// exist. Written to be acted on: a model told only "refused" tries again the
/// same way, and this is the one storage error it will hit most.
fn scope_hint(path: &str) -> StorageError {
    StorageError::Refused(format!(
        "paths start with session/, agent/ or workspace/. For something you are working on \
         now use session/{0}; for something this agent should keep use agent/{0}; for \
         something the whole workspace shares use workspace/{0}.",
        path.trim_start_matches('/')
    ))
}

/// Splits a guest path into its scope and the rest, or explains why not.
pub fn split(requested: &str) -> Result<(Scope, &str), StorageError> {
    let path = requested.trim().trim_start_matches("./");
    let (head, rest) = match path.split_once('/') {
        Some((h, r)) => (h, r),
        None => (path, ""),
    };
    let scope = Scope::parse(head).ok_or_else(|| scope_hint(path))?;
    Ok((scope, rest))
}

/// Resolves a guest-supplied path to a real key, or refuses.
///
/// Refuses rather than repairs. Clamping a traversal back inside the root
/// silently changes what was asked for, which turns "read the wrong file" into
/// "read a different file and report success" -- and the caller never learns
/// its path was wrong.
pub fn resolve(space: &Space, requested: &str) -> Result<String, StorageError> {
    let (scope, rest) = split(requested)?;
    let rest = clean(rest)?;
    if rest.is_empty() {
        return Err(StorageError::Refused(format!(
            "{}/ is a folder; name a file inside it",
            scope.as_str()
        )));
    }
    let resolved = format!("{}{}", root_for(space, scope), rest);
    // Belt and braces. `clean` should make this unreachable, but this is the
    // check that actually matters, and it costs a comparison.
    if !resolved.starts_with(&root_for(space, scope)) {
        return Err(StorageError::PermissionDenied);
    }
    Ok(resolved)
}

/// Resolves a guest prefix for listing: a scope alone, or a scope and a
/// folder within it. Empty means every scope, which the caller handles.
pub fn resolve_prefix(space: &Space, requested: &str) -> Result<String, StorageError> {
    let (scope, rest) = split(requested)?;
    let rest = clean(rest)?;
    Ok(format!("{}{}", root_for(space, scope), rest))
}

/// The part of a path after the scope, with the sloppiness removed and the
/// hostility refused.
fn clean(rest: &str) -> Result<String, StorageError> {
    // A leading slash here is a doubled separator after the scope
    // ("session//x"), which is sloppy rather than hostile: an absolute path
    // never gets this far, because "" is not a scope.
    // Backslashes would be a separator on some platforms and a literal on
    // others; a path whose meaning depends on where it is read is not one to
    // guess about. Control characters and nulls end up truncating keys in
    // ways that vary by store.
    if rest.contains('\\') || rest.chars().any(|c| c.is_control()) {
        return Err(StorageError::PermissionDenied);
    }
    let mut parts: Vec<&str> = Vec::new();
    for component in rest.split('/') {
        match component {
            // Skip rather than refuse: doubled separators and a trailing
            // slash are sloppy, not hostile.
            "" | "." => continue,
            ".." => return Err(StorageError::PermissionDenied),
            other => parts.push(other),
        }
    }
    let mut joined = parts.join("/");
    if rest.ends_with('/') && !joined.is_empty() {
        joined.push('/');
    }
    Ok(joined)
}

/// Turns a real key back into the path a guest would name it by, or None if
/// it belongs to no scope of this space -- which a listing should never
/// produce, but a listing is somebody else's answer.
pub fn strip_root(space: &Space, stored: &str) -> Option<String> {
    for scope in Scope::ALL {
        if let Some(rest) = stored.strip_prefix(&root_for(space, scope)) {
            return Some(format!("{}/{}", scope.as_str(), rest));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> Space {
        Space {
            workspace_id: Uuid::parse_str("01a06545-c926-7672-ae22-5971b4871bfd").unwrap(),
            agent_id: Uuid::parse_str("01a06545-c926-7672-ae22-5971b4871aaa").unwrap(),
            session_id: Uuid::parse_str("01a06545-c926-7672-ae22-5971b4871bbb").unwrap(),
        }
    }

    #[test]
    fn each_scope_lands_under_its_own_prefix() {
        let s = space();
        assert_eq!(
            resolve(&s, "workspace/reports/q3.csv").unwrap(),
            "workspaces/01a06545-c926-7672-ae22-5971b4871bfd/reports/q3.csv"
        );
        assert_eq!(
            resolve(&s, "agent/procedures.md").unwrap(),
            "agents/01a06545-c926-7672-ae22-5971b4871bfd/01a06545-c926-7672-ae22-5971b4871aaa/procedures.md"
        );
        assert_eq!(
            resolve(&s, "session/scratch.txt").unwrap(),
            "sessions/01a06545-c926-7672-ae22-5971b4871bfd/01a06545-c926-7672-ae22-5971b4871aaa/01a06545-c926-7672-ae22-5971b4871bbb/scratch.txt"
        );
    }

    #[test]
    fn a_path_without_a_scope_is_told_how_to_write_one() {
        let err = resolve(&space(), "notes.md").expect_err("scopeless");
        let text = err.to_string();
        assert!(text.contains("session/notes.md"), "{text}");
        assert!(text.contains("agent/notes.md"), "{text}");
        assert!(text.contains("workspace/notes.md"), "{text}");
    }

    #[test]
    fn traversal_is_refused_however_it_is_spelled() {
        for attempt in [
            "session/../workspace/secrets",
            "agent/reports/../../other/secrets",
            "workspace/..",
            "session/a/b/../../../c",
        ] {
            assert!(
                resolve(&space(), attempt).is_err(),
                "{attempt:?} should be refused, not repaired"
            );
        }
    }

    #[test]
    fn other_ways_out_are_refused_too() {
        for attempt in ["/etc/passwd", "", "   ", "/", "session/reports\\q3.csv", "session/q3\0.csv"] {
            assert!(resolve(&space(), attempt).is_err(), "{attempt:?} should be refused");
        }
    }

    #[test]
    fn a_scope_alone_is_a_folder_not_a_file() {
        assert!(resolve(&space(), "session").is_err());
        assert!(resolve(&space(), "session/").is_err());
        assert!(resolve_prefix(&space(), "session/").is_ok());
        assert!(resolve_prefix(&space(), "agent").is_ok());
    }

    #[test]
    fn sloppiness_is_tolerated_where_it_is_unambiguous() {
        let s = space();
        assert_eq!(
            resolve(&s, "./session//reports/q3.csv").unwrap(),
            resolve(&s, "session/reports/q3.csv").unwrap()
        );
    }

    #[test]
    fn a_workspace_cannot_reach_another_by_naming_it() {
        let s = space();
        let resolved = resolve(&s, "workspace/workspaces/00000000-0000-0000-0000-000000000000/x").unwrap();
        assert!(resolved.starts_with(&root_for(&s, Scope::Workspace)));
    }

    #[test]
    fn stripping_gives_back_the_scoped_path() {
        let s = space();
        let stored = resolve(&s, "agent/reports/q3.csv").unwrap();
        assert_eq!(strip_root(&s, &stored).unwrap(), "agent/reports/q3.csv");
        assert_eq!(strip_root(&s, "workspaces/somebody-else/x"), None);
    }
}
