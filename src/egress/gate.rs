//! Which outbound requests a turn may not make without somebody's word.
//!
//! See `docs/approvals.md`. The rule is declared in the frontmatter of the skill
//! file that documents an operation; this is how it reaches the only tier that
//! can act on it.
//!
//! The gateway is that tier, and the reasons are the ones `egress/mod.rs` gives
//! for the request being *made* there: it is the only place that sees every
//! outbound call, holds the credential, can refuse before money moves, and
//! cannot be influenced by a compromised runtime. What it does not have is any
//! idea which *operation* a request is. It receives a method, a URL and a body;
//! the file that declared the rule lives in the API, and the runtime that read
//! it is the tier being defended against.
//!
//! So the rule travels the way egress rules already do. The API resolves a
//! turn's bound skills, commits to the matchers it found, and signs the root into
//! the turn token. A request offers the matcher it believes applies together with
//! a proof, and the gateway checks it against the root inside the token -- so a
//! runtime that rewrote its own copy gets nowhere, and a claim that was stripped
//! reads as "could not verify", which means refused.
//!
//! The asymmetry with egress is worth stating, because it is the whole design.
//! An egress rule is a *permission*: a request proves one and is allowed, and
//! failing to prove one means refused. A gate is an *obligation*: a request that
//! matches one is refused until somebody approves, and failing to prove the
//! absence of a gate cannot be the safe direction, because absence is not
//! provable from a Merkle root. So the gateway does not ask "is this gated"; the
//! API tells it what is gated for this turn, and the gateway refuses anything
//! that matches. A stripped commitment therefore has to mean *every* request is
//! gated rather than none, which is what `Gates::none()` versus a missing claim
//! distinguishes.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::commit::Hash;

const TAG_LEAF: &[u8] = b"outturn:gate:leaf:v1\0";
const TAG_NODE: &[u8] = b"outturn:gate:node:v1\0";
const TAG_EMPTY: &[u8] = b"outturn:gate:empty:v1\0";

/// A request shape that needs approving, as the API commits to it.
///
/// Matched on method and path rather than on an operation name, because the
/// gateway sees a request and not an operation. The host comes from the skill's
/// declared hosts, so a gate for `POST /charges` on the guesthouse does not gate
/// the same path on somebody else's API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    /// The act, which is what a queue row and a grant are about. Carried so the
    /// gateway can say what is being asked for without reading a skill.
    pub requires: String,
    pub host: String,
    /// Upper case, as the request's own method is normalised to.
    pub method: String,
    /// The path this applies to. A trailing `*` matches anything below it, which
    /// is how an operation whose path carries an id is expressed.
    pub path: String,
    /// The field of the request body that names the unit a wider grant may span.
    /// Absent when the declaration offered no `covers`.
    pub identified_by: Option<String>,
}

impl Gate {
    /// Whether this gate is about the request in hand.
    ///
    /// Host and method exactly; path with one wildcard at the end, because an
    /// operation on `/bookings/{id}` is one rule and not one per booking. No
    /// wildcard anywhere else: a pattern that can match in the middle is a
    /// pattern somebody writes `*` into and gates more than they meant.
    pub fn covers_request(&self, host: &str, method: &str, path: &str) -> bool {
        // Through the same matcher an egress rule uses, because the host stored
        // here came from the skill's declared hosts and those go through
        // `normalise_host`, which *accepts wildcards*. An exact compare -- which
        // this was -- meant a skill declaring `*.stripe.com` stored a gate on
        // `*.stripe.com`, the gateway matched it against `api.stripe.com`, and the
        // charge went out unapproved. Not a forgery: an ordinary configuration
        // that silently produced an ungated operation, which is the failure this
        // whole mechanism is arranged against. Trailing dots and case come free.
        if !crate::runtime::egress::host_matches(&self.host, host)
            || !method.eq_ignore_ascii_case(&self.method)
        {
            return false;
        }
        let path = normalise(path);
        let pattern = normalise(&self.path);
        match pattern.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => path == pattern,
        }
    }
}

/// A path as the gate compares it.
///
/// Matching the raw path is what a bypass looks for, and the forms are cheap to
/// find: `//charges`, `/charges/` and `/char%67es` all reach the same endpoint on
/// plenty of servers while comparing unequal to `/charges`. Whether a *particular*
/// recipient routes them there is the recipient's business, and a gate that
/// depended on that would be a gate whose strength varied by whose API it was
/// pointed at.
///
/// So: percent-decode, collapse repeated slashes, and drop one trailing slash.
/// `reqwest::Url::parse` has already resolved `.` and `..` by the time a path
/// reaches here, which is the one normalisation this does not have to do.
///
/// Deliberately not case-folding. Paths are case-sensitive in HTTP and on most
/// servers, so folding them would gate `/Charges` as well and refuse work nobody
/// meant to gate -- and the failure direction there is a conversation that cannot
/// proceed rather than a charge that slips through.
fn normalise(path: &str) -> String {
    let decoded = percent_decode(path);
    let mut out = String::with_capacity(decoded.len());
    let mut last_was_slash = false;
    for c in decoded.chars() {
        if c == '/' {
            if last_was_slash {
                continue;
            }
            last_was_slash = true;
        } else {
            last_was_slash = false;
        }
        out.push(c);
    }
    // One trailing slash, and only where something precedes it: `/` itself is a
    // path and must not become empty.
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Decodes `%xx` escapes, leaving anything malformed as it stands.
///
/// A truncated or non-hex escape is left alone rather than dropped: it is not a
/// character the sender could have meant, and inventing one would be a second
/// spelling of a path this has to compare exactly.
fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // Invalid UTF-8 cannot be a path anybody typed, and replacing it keeps this
    // comparing strings rather than failing.
    String::from_utf8_lossy(&out).into_owned()
}

/// What a turn is gated by, as the gateway knows it.
///
/// Held as the whole set rather than proven one at a time, which is the opposite
/// of how egress rules travel and for the reason in the module comment: a
/// permission is proven by the request that wants it, and an obligation has to be
/// known in full before a request can be said not to match any of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gates(Vec<Gate>);

impl Gates {
    /// A turn nothing gates.
    ///
    /// Distinct from a missing commitment, which is refused. This is the API
    /// saying "I looked and there are none", and it is signed.
    pub fn none() -> Self {
        Self(Vec::new())
    }

    pub fn of(mut gates: Vec<Gate>) -> Self {
        // Sorted here rather than relied on from a query. Which of two overlapping
        // gates `covering` finds decides the wording of a refusal, and a wording
        // that varies by pod is one nobody can reproduce.
        gates.sort_by(|a, b| {
            (&a.host, &a.method, &a.path, &a.requires).cmp(&(
                &b.host,
                &b.method,
                &b.path,
                &b.requires,
            ))
        });
        Self(gates)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The gate that covers this request, if any.
    ///
    /// Two gates covering one request is not an error: a workspace binding two
    /// skills that both document the same endpoint is ordinary, and refusing would
    /// make a conversation impossible rather than gated. Either answer refuses the
    /// request, so which one is found decides only the `requires` in the message.
    ///
    /// Sorted on construction so that choice is the same on every pod. It was not,
    /// and the order happened to be deterministic because `gates_for_turn` reads
    /// them ordered -- deterministic by accident of a query somebody may
    /// reasonably change, which is the same reason `leaves` sorts rather than
    /// assuming.
    pub fn covering(&self, host: &str, method: &str, path: &str) -> Option<&Gate> {
        self.0
            .iter()
            .find(|gate| gate.covers_request(host, method, path))
    }

    /// The root the API signs, over every gate a turn carries.
    pub fn root(&self, workspace_id: Uuid) -> Hash {
        root_of(&self.leaves(workspace_id))
    }

    /// Whether this set is the one a root was taken over.
    ///
    /// The gateway is sent the set and checks it against the token, rather than
    /// being sent one gate and a path to the root. It needs the whole set to
    /// conclude that a request matches none of it, and a proof of absence is not
    /// something a Merkle tree provides.
    pub fn matches(&self, workspace_id: Uuid, committed: &Hash) -> bool {
        self.root(workspace_id).matches(committed)
    }

    /// Leaves in the order the tree is built over: sorted and deduplicated, so
    /// the API and the gateway agree byte for byte whatever order the skills
    /// were read in.
    fn leaves(&self, workspace_id: Uuid) -> Vec<Hash> {
        let mut leaves: Vec<Hash> = self.0.iter().map(|g| leaf(workspace_id, g)).collect();
        leaves.sort_unstable_by_key(|h| h.0);
        leaves.dedup_by(|a, b| a.0 == b.0);
        leaves
    }
}

/// One gate, hashed with its workspace.
///
/// The field loop is `commit::hash_fields`, shared rather than retyped: two copies
/// of it is how one ends up with a different byte order, which is precisely what
/// happened here before this -- big-endian lengths where the egress leaf is
/// little-endian. Harmless while each tree only ever compares against itself, and
/// the signature of copied hashing code regardless.
fn leaf(workspace_id: Uuid, gate: &Gate) -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_LEAF);
    h.update(workspace_id.as_bytes());
    super::commit::hash_fields(
        &mut h,
        [
            Some(gate.requires.as_str()),
            Some(gate.host.as_str()),
            Some(gate.method.as_str()),
            Some(gate.path.as_str()),
            gate.identified_by.as_deref(),
        ],
    );
    Hash(h.finalize().into())
}

/// The tree, under this module's own tags.
///
/// One implementation, in `commit`, because the shape is a security choice rather
/// than a detail: an odd level is carried up rather than duplicated, and
/// duplicating would let a reduced set hash to the committed root. On this side
/// that is a gate vanishing, which is a charge going out unapproved -- so a fix to
/// one tree and not the other is worse here than there. The tags are what keep the
/// two commitments from vouching for each other.
fn root_of(leaves: &[Hash]) -> Hash {
    super::commit::root_with(TAG_NODE, TAG_EMPTY, leaves)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(requires: &str, method: &str, path: &str) -> Gate {
        Gate {
            requires: requires.into(),
            host: "outturn-hollowbrook".into(),
            method: method.into(),
            path: path.into(),
            identified_by: Some("booking_id".into()),
        }
    }

    fn workspace() -> Uuid {
        Uuid::from_bytes([7; 16])
    }

    #[test]
    fn a_gate_covers_its_own_method_and_path() {
        let g = gate("charge", "POST", "/charges");
        assert!(g.covers_request("outturn-hollowbrook", "POST", "/charges"));
        assert!(!g.covers_request("outturn-hollowbrook", "GET", "/charges"));
        assert!(!g.covers_request("outturn-hollowbrook", "POST", "/bookings"));
        assert!(!g.covers_request("example.com", "POST", "/charges"));
    }

    #[test]
    fn the_host_and_method_are_matched_without_regard_to_case() {
        let g = gate("charge", "post", "/charges");
        assert!(g.covers_request("OUTTURN-HOLLOWBROOK", "POST", "/charges"));
    }

    #[test]
    fn a_trailing_star_matches_below_it() {
        // An operation on `/bookings/{id}` is one rule, not one per booking.
        let g = gate("refund", "DELETE", "/bookings/*");
        assert!(g.covers_request("outturn-hollowbrook", "DELETE", "/bookings/bk_1"));
        assert!(!g.covers_request("outturn-hollowbrook", "DELETE", "/charges/c_1"));
    }

    #[test]
    fn a_star_anywhere_else_is_literal() {
        // A pattern that can match in the middle is one somebody gates more with
        // than they meant.
        let g = gate("charge", "POST", "/char*ges");
        assert!(!g.covers_request("outturn-hollowbrook", "POST", "/charges"));
        assert!(g.covers_request("outturn-hollowbrook", "POST", "/char*ges"));
    }

    #[test]
    fn the_set_finds_the_gate_covering_a_request() {
        let gates = Gates::of(vec![
            gate("charge", "POST", "/charges"),
            gate("refund", "DELETE", "/bookings/*"),
        ]);
        assert_eq!(
            gates
                .covering("outturn-hollowbrook", "POST", "/charges")
                .map(|g| g.requires.as_str()),
            Some("charge")
        );
        assert_eq!(gates.covering("outturn-hollowbrook", "GET", "/rooms"), None);
    }

    #[test]
    fn a_set_matches_the_root_it_was_committed_over() {
        let gates = Gates::of(vec![gate("charge", "POST", "/charges")]);
        let root = gates.root(workspace());
        assert!(gates.matches(workspace(), &root));
    }

    #[test]
    fn order_does_not_change_the_root() {
        // The API reads skills in whatever order its query returns, and the
        // gateway must agree byte for byte.
        let a = gate("charge", "POST", "/charges");
        let b = gate("refund", "DELETE", "/bookings/*");
        assert_eq!(
            Gates::of(vec![a.clone(), b.clone()]).root(workspace()).0,
            Gates::of(vec![b, a]).root(workspace()).0
        );
    }

    #[test]
    fn a_repeated_gate_does_not_change_the_root() {
        // Two bound skills documenting the same endpoint is ordinary.
        let g = gate("charge", "POST", "/charges");
        assert_eq!(
            Gates::of(vec![g.clone()]).root(workspace()).0,
            Gates::of(vec![g.clone(), g]).root(workspace()).0
        );
    }

    #[test]
    fn an_added_gate_changes_the_root() {
        let one = Gates::of(vec![gate("charge", "POST", "/charges")]);
        let two = Gates::of(vec![
            gate("charge", "POST", "/charges"),
            gate("refund", "DELETE", "/bookings/*"),
        ]);
        assert!(!two.matches(workspace(), &one.root(workspace())));
    }

    #[test]
    fn a_set_does_not_match_another_workspaces_root() {
        // Otherwise a gate committed for one tenant vouches for another's.
        let gates = Gates::of(vec![gate("charge", "POST", "/charges")]);
        let elsewhere = Uuid::from_bytes([9; 16]);
        assert!(!gates.matches(elsewhere, &gates.root(workspace())));
    }

    #[test]
    fn the_empty_set_has_a_root_of_its_own() {
        // "Nothing gates this turn" is a statement the API signs, not the absence
        // of one -- a stripped claim must not read as every gate vanishing.
        let none = Gates::none();
        assert!(none.matches(workspace(), &none.root(workspace())));
        assert_ne!(none.root(workspace()).0, [0u8; 32]);
    }

    /// A set that is a subset of another does not share its root.
    ///
    /// The property the odd-level carry exists for, stated in the form this side
    /// can state it. `commit.rs` tests it as "a duplicated leaf does not forge a
    /// second set", which cannot be written here: `Gates::leaves` deduplicates, so
    /// `[a, b, c, c]` *is* `[a, b, c]` by construction and the two roots are
    /// equal on purpose -- two bound skills documenting one endpoint is ordinary.
    ///
    /// What must not happen is a *smaller* set hashing to a larger one's root,
    /// because that is a runtime presenting a reduced gate set and a gate that
    /// vanishes is a charge going out unapproved. On the egress side the same
    /// forgery only over-permits a host that was in the real set.
    #[test]
    fn a_reduced_set_does_not_hash_to_the_committed_root() {
        let a = gate("charge", "POST", "/charges");
        let b = gate("refund", "DELETE", "/bookings/*");
        let c = gate("comp", "POST", "/comps");
        let all = Gates::of(vec![a.clone(), b.clone(), c.clone()]);
        let committed = all.root(workspace());

        // Every way of dropping one, at each position, including the odd end
        // where a duplicating tree would go wrong.
        for reduced in [
            Gates::of(vec![b.clone(), c.clone()]),
            Gates::of(vec![a.clone(), c.clone()]),
            Gates::of(vec![a.clone(), b.clone()]),
            Gates::of(vec![a.clone()]),
            Gates::none(),
        ] {
            assert!(
                !reduced.matches(workspace(), &committed),
                "a reduced set passed the commitment"
            );
        }
        assert!(all.matches(workspace(), &committed));
    }

    /// Every size up to a few levels, so the carry is exercised at each odd one.
    #[test]
    fn a_tree_of_any_size_has_a_root_of_its_own() {
        let mut seen: Vec<[u8; 32]> = Vec::new();
        for n in 0..9 {
            let gates = Gates::of(
                (0..n)
                    .map(|i| gate(&format!("act{i}"), "POST", &format!("/p{i}")))
                    .collect(),
            );
            let root = gates.root(workspace()).0;
            assert!(
                !seen.contains(&root),
                "a set of {n} hashed to the same root as a smaller one"
            );
            assert!(gates.matches(workspace(), &Hash(root)));
            seen.push(root);
        }
    }

    #[test]
    fn the_empty_root_is_not_a_populated_one() {
        let none = Gates::none();
        let some = Gates::of(vec![gate("charge", "POST", "/charges")]);
        assert!(!some.matches(workspace(), &none.root(workspace())));
        assert!(!none.matches(workspace(), &some.root(workspace())));
    }

    #[test]
    fn fields_cannot_be_slid_between_each_other() {
        // Without length prefixes, `host: "ab", path: "c"` and `host: "a", path:
        // "bc"` hash alike and one gate stands in for another.
        let mut left = gate("charge", "POST", "/charges");
        left.host = "ab".into();
        left.path = "c".into();
        let mut right = gate("charge", "POST", "/charges");
        right.host = "a".into();
        right.path = "bc".into();
        assert_ne!(
            Gates::of(vec![left]).root(workspace()).0,
            Gates::of(vec![right]).root(workspace()).0
        );
    }

    #[test]
    fn an_absent_identified_by_is_not_an_empty_one() {
        let mut absent = gate("charge", "POST", "/charges");
        absent.identified_by = None;
        let mut empty = gate("charge", "POST", "/charges");
        empty.identified_by = Some(String::new());
        assert_ne!(
            Gates::of(vec![absent]).root(workspace()).0,
            Gates::of(vec![empty]).root(workspace()).0
        );
    }
}

#[cfg(test)]
mod bypasses {
    use super::*;

    fn gate() -> Gate {
        Gate {
            requires: "charge".into(),
            host: "outturn-hollowbrook".into(),
            method: "POST".into(),
            path: "/charges".into(),
            identified_by: None,
        }
    }

    /// Every spelling of the gated path is still the gated path.
    ///
    /// Found by probing `reqwest::Url::parse`, which resolves `.` and `..` and
    /// leaves the rest: `//charges`, `/charges/` and `/char%67es` all came through
    /// with paths that compared unequal to `/charges`. Whether a particular
    /// recipient routes those to the same handler is the recipient's business --
    /// this fixture happens not to -- and a gate whose strength depended on that
    /// would be no gate at all.
    #[test]
    fn a_differently_spelled_path_is_still_gated() {
        let g = gate();
        for path in [
            "/charges",
            "//charges",
            "/charges/",
            "///charges//",
            "/char%67es",
            "/%63harges",
        ] {
            assert!(
                g.covers_request("outturn-hollowbrook", "POST", path),
                "{path} slipped past the gate"
            );
        }
    }

    #[test]
    fn a_different_path_is_still_not_gated() {
        // Normalising must not widen the gate onto endpoints it does not name.
        let g = gate();
        for path in ["/bookings", "/chargesx", "/charges2", "/rooms/charges"] {
            assert!(
                !g.covers_request("outturn-hollowbrook", "POST", path),
                "{path} was gated and should not be"
            );
        }
    }

    #[test]
    fn case_is_not_folded() {
        // Paths are case-sensitive in HTTP. Folding would gate `/Charges` too and
        // refuse work nobody meant to gate; the cost of not folding is a
        // recipient that routes case-insensitively, which is rare and whose
        // failure is a charge going out rather than a conversation stuck.
        assert!(
            !gate().covers_request("outturn-hollowbrook", "POST", "/CHARGES"),
            "case was folded"
        );
    }

    #[test]
    fn a_wildcard_normalises_too() {
        let mut g = gate();
        g.path = "/bookings/*".into();
        assert!(g.covers_request("outturn-hollowbrook", "POST", "//bookings/bk_1"));
        assert!(g.covers_request("outturn-hollowbrook", "POST", "/bookings/bk_1/"));
    }

    #[test]
    fn a_malformed_escape_is_left_alone() {
        // `%zz` is not a character anybody meant, and inventing one would be a
        // second spelling of a path this compares exactly.
        let mut g = gate();
        g.path = "/char%zzges".into();
        assert!(g.covers_request("outturn-hollowbrook", "POST", "/char%zzges"));
    }

    #[test]
    fn the_root_path_survives_normalising() {
        let mut g = gate();
        g.path = "/".into();
        assert!(g.covers_request("outturn-hollowbrook", "POST", "/"));
        assert!(g.covers_request("outturn-hollowbrook", "POST", "//"));
    }
}

#[cfg(test)]
mod hosts {
    use super::*;

    /// A wildcard host gates the hosts it covers.
    ///
    /// The bug this replaced: `covers_request` compared hosts exactly, and the
    /// host stored on a gate comes from the skill's declared hosts -- which go
    /// through `normalise_host` and may be `*.stripe.com`. So a workspace
    /// declaring a wildcard published a skill whose file said an operation was
    /// gated, and the gateway, matching `api.stripe.com` against `*.stripe.com`,
    /// found no gate and let the charge out. Ordinary configuration, not an
    /// attack, which is what made it worth finding.
    ///
    /// Every fixture in the suite had used the rule as typed -- port and all --
    /// on both sides of the comparison, so they agreed with each other and never
    /// exercised a real host spelling.
    #[test]
    fn a_wildcard_host_gates_what_it_covers() {
        let g = Gate {
            requires: "charge".into(),
            host: "*.stripe.com".into(),
            method: "POST".into(),
            path: "/v1/charges".into(),
            identified_by: None,
        };
        assert!(g.covers_request("api.stripe.com", "POST", "/v1/charges"));
        assert!(g.covers_request("files.stripe.com", "POST", "/v1/charges"));
        // The dot is part of the suffix, as in the egress rule it borrows.
        assert!(!g.covers_request("notstripe.com", "POST", "/v1/charges"));
        assert!(!g.covers_request("stripe.com.evil.test", "POST", "/v1/charges"));
    }

    #[test]
    fn an_exact_host_still_gates_only_itself() {
        let g = Gate {
            requires: "charge".into(),
            host: "outturn-hollowbrook".into(),
            method: "POST".into(),
            path: "/charges".into(),
            identified_by: None,
        };
        assert!(g.covers_request("outturn-hollowbrook", "POST", "/charges"));
        assert!(!g.covers_request("sub.outturn-hollowbrook", "POST", "/charges"));
        assert!(!g.covers_request("example.com", "POST", "/charges"));
    }

    #[test]
    fn a_trailing_dot_and_case_do_not_evade_a_gate() {
        let g = Gate {
            requires: "charge".into(),
            host: "api.stripe.com".into(),
            method: "POST".into(),
            path: "/v1/charges".into(),
            identified_by: None,
        };
        for host in ["api.stripe.com.", "API.STRIPE.COM", "Api.Stripe.Com."] {
            assert!(
                g.covers_request(host, "POST", "/v1/charges"),
                "{host} evaded the gate"
            );
        }
    }
}
