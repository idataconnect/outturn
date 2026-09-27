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

use super::commit::{Hash, Proof, Step};

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
        if !host.eq_ignore_ascii_case(&self.host) || !method.eq_ignore_ascii_case(&self.method) {
            return false;
        }
        match self.path.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => path == self.path,
        }
    }
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

    pub fn of(gates: Vec<Gate>) -> Self {
        Self(gates)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The gate that covers this request, if any.
    ///
    /// The first match wins, and the set is sorted, so two gates that both cover
    /// a request resolve the same way on every pod. Two gates covering one
    /// request is not an error: a workspace binding two skills that both document
    /// the same endpoint is ordinary, and refusing would make a conversation
    /// impossible rather than gated.
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
/// Length-prefixed field by field, as the egress leaf is: without it `host: "a",
/// path: "bc"` and `host: "ab", path: "c"` hash alike, and one gate stands in for
/// another.
fn leaf(workspace_id: Uuid, gate: &Gate) -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_LEAF);
    h.update(workspace_id.as_bytes());
    for field in [
        Some(gate.requires.as_str()),
        Some(gate.host.as_str()),
        Some(gate.method.as_str()),
        Some(gate.path.as_str()),
        gate.identified_by.as_deref(),
    ] {
        // A present-but-empty field and an absent one are different gates.
        match field {
            Some(value) => {
                h.update([1u8]);
                h.update((value.len() as u64).to_be_bytes());
                h.update(value.as_bytes());
            }
            None => h.update([0u8]),
        }
    }
    Hash(h.finalize().into())
}

fn node(left: &Hash, right: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_NODE);
    h.update(left.0);
    h.update(right.0);
    Hash(h.finalize().into())
}

/// The empty set, with a tag of its own.
///
/// So that "this turn is gated by nothing" is a statement the API signs rather
/// than the absence of one. A stripped claim is the answer an attacker would
/// choose, and here it would mean every gate vanished.
fn empty_root() -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_EMPTY);
    Hash(h.finalize().into())
}

/// Carries an odd level up rather than duplicating its last node, as
/// `commit::root_of` does and for the same reason: duplicating lets a different
/// set hash to the same root.
fn root_of(leaves: &[Hash]) -> Hash {
    if leaves.is_empty() {
        return empty_root();
    }
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut pairs = level.chunks_exact(2);
        for pair in &mut pairs {
            next.push(node(&pair[0], &pair[1]));
        }
        if let [odd] = pairs.remainder() {
            next.push(*odd);
        }
        level = next;
    }
    level[0]
}

/// Unused, but kept honest: `Proof` and `Step` are the egress shape, and gates
/// deliberately do not use them. Named here so a reader who goes looking finds
/// the reason rather than an omission.
#[allow(dead_code)]
fn why_no_proof(_: Option<Proof>, _: Option<Step>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(requires: &str, method: &str, path: &str) -> Gate {
        Gate {
            requires: requires.into(),
            host: "outturn-hollowbrook:8084".into(),
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
        assert!(g.covers_request("outturn-hollowbrook:8084", "POST", "/charges"));
        assert!(!g.covers_request("outturn-hollowbrook:8084", "GET", "/charges"));
        assert!(!g.covers_request("outturn-hollowbrook:8084", "POST", "/bookings"));
        assert!(!g.covers_request("example.com", "POST", "/charges"));
    }

    #[test]
    fn the_host_and_method_are_matched_without_regard_to_case() {
        let g = gate("charge", "post", "/charges");
        assert!(g.covers_request("OUTTURN-HOLLOWBROOK:8084", "POST", "/charges"));
    }

    #[test]
    fn a_trailing_star_matches_below_it() {
        // An operation on `/bookings/{id}` is one rule, not one per booking.
        let g = gate("refund", "DELETE", "/bookings/*");
        assert!(g.covers_request("outturn-hollowbrook:8084", "DELETE", "/bookings/bk_1"));
        assert!(!g.covers_request("outturn-hollowbrook:8084", "DELETE", "/charges/c_1"));
    }

    #[test]
    fn a_star_anywhere_else_is_literal() {
        // A pattern that can match in the middle is one somebody gates more with
        // than they meant.
        let g = gate("charge", "POST", "/char*ges");
        assert!(!g.covers_request("outturn-hollowbrook:8084", "POST", "/charges"));
        assert!(g.covers_request("outturn-hollowbrook:8084", "POST", "/char*ges"));
    }

    #[test]
    fn the_set_finds_the_gate_covering_a_request() {
        let gates = Gates::of(vec![
            gate("charge", "POST", "/charges"),
            gate("refund", "DELETE", "/bookings/*"),
        ]);
        assert_eq!(
            gates
                .covering("outturn-hollowbrook:8084", "POST", "/charges")
                .map(|g| g.requires.as_str()),
            Some("charge")
        );
        assert_eq!(
            gates.covering("outturn-hollowbrook:8084", "GET", "/rooms"),
            None
        );
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
