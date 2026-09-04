//! Mapping what a guest asks for onto where it actually lives.
//!
//! A guest names `reports/q3.csv` and never learns which tenant it belongs to.
//! That is the point: a component cannot get a tenant wrong if it is never
//! told one, and cannot reach another's data by constructing a path, because
//! the root it is given is not a path it can escape from.
//!
//! Tenants are prefixes in one bucket rather than a bucket each. Buckets are a
//! limited resource -- a hundred per AWS account by default, a thousand at the
//! ceiling -- and a limit on buckets would become a limit on customers.

use uuid::Uuid;

use super::StorageError;

/// Where a tenant's objects live.
pub fn root_for(tenant_id: Uuid) -> String {
    format!("tenants/{tenant_id}/")
}

/// Resolves a guest-supplied path to a real one, or refuses.
///
/// Refuses rather than repairs. Clamping a traversal back inside the root
/// silently changes what was asked for, which turns "read the wrong file" into
/// "read a different file and report success" -- and the caller never learns
/// its path was wrong.
pub fn resolve(tenant_id: Uuid, requested: &str) -> Result<String, StorageError> {
    let path = requested.trim();

    if path.is_empty() {
        return Err(StorageError::PermissionDenied);
    }

    // Absolute paths are a different namespace than the one on offer, and a
    // guest asking for one has misunderstood rather than mistyped.
    if path.starts_with('/') {
        return Err(StorageError::PermissionDenied);
    }

    // Backslashes would be a separator on some platforms and a literal on
    // others; a path whose meaning depends on where it is read is not one to
    // guess about. Control characters and nulls end up truncating keys in
    // ways that vary by store.
    if path.contains('\\') || path.chars().any(|c| c.is_control()) {
        return Err(StorageError::PermissionDenied);
    }

    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            // Skip rather than refuse: doubled separators and a trailing
            // slash are sloppy, not hostile.
            "" | "." => continue,
            ".." => return Err(StorageError::PermissionDenied),
            other => parts.push(other),
        }
    }

    if parts.is_empty() {
        return Err(StorageError::PermissionDenied);
    }

    let resolved = format!("{}{}", root_for(tenant_id), parts.join("/"));

    // Belt and braces. The loop above should make this unreachable, but this
    // is the check that actually matters, and it costs a comparison.
    if !resolved.starts_with(&root_for(tenant_id)) {
        return Err(StorageError::PermissionDenied);
    }

    Ok(resolved)
}

/// Strips the tenant root back off, for showing a guest what it asked about.
///
/// A listing that returned real keys would leak the tenant's identity and the
/// layout of the store into a component that is deliberately not told either.
pub fn strip_root(tenant_id: Uuid, stored: &str) -> String {
    stored
        .strip_prefix(&root_for(tenant_id))
        .unwrap_or(stored)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant() -> Uuid {
        Uuid::parse_str("01a06545-c926-7672-ae22-5971b4871bfd").unwrap()
    }

    #[test]
    fn a_plain_path_lands_under_the_tenant() {
        let resolved = resolve(tenant(), "reports/q3.csv").expect("allowed");
        assert_eq!(
            resolved,
            "tenants/01a06545-c926-7672-ae22-5971b4871bfd/reports/q3.csv"
        );
    }

    #[test]
    fn traversal_is_refused_however_it_is_spelled() {
        for attempt in [
            "../other/secrets",
            "reports/../../other/secrets",
            "reports/../..",
            "..",
            "a/b/../../../c",
        ] {
            assert!(
                resolve(tenant(), attempt).is_err(),
                "{attempt:?} should be refused, not repaired"
            );
        }
    }

    #[test]
    fn other_ways_out_are_refused_too() {
        for attempt in [
            "/etc/passwd",
            "",
            "   ",
            "/",
            "reports\\q3.csv",
            "reports/q3\0.csv",
            "reports/\nq3.csv",
        ] {
            assert!(resolve(tenant(), attempt).is_err(), "{attempt:?} should be refused");
        }
    }

    #[test]
    fn sloppiness_is_tolerated_where_it_is_unambiguous() {
        // Doubled separators, a leading dot-slash and a trailing slash all
        // mean one thing, so they are cleaned rather than rejected.
        assert_eq!(
            resolve(tenant(), "./reports//q3.csv").expect("allowed"),
            resolve(tenant(), "reports/q3.csv").expect("allowed")
        );
    }

    #[test]
    fn a_tenant_cannot_reach_another_by_naming_it() {
        // The tenant's own root is not a path a guest can write; naming it
        // just nests one inside the other.
        let resolved = resolve(tenant(), "tenants/00000000-0000-0000-0000-000000000000/x")
            .expect("allowed");
        assert!(resolved.starts_with(&root_for(tenant())));
        assert!(resolved.ends_with(
            "tenants/01a06545-c926-7672-ae22-5971b4871bfd/tenants/00000000-0000-0000-0000-000000000000/x"
        ));
    }

    #[test]
    fn stripping_hides_where_things_really_live() {
        let stored = resolve(tenant(), "reports/q3.csv").expect("allowed");
        assert_eq!(strip_root(tenant(), &stored), "reports/q3.csv");
    }
}
