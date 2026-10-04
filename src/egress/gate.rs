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

/// The methods this platform will send.
///
/// One list, because there were three: the gateway's match arm, the frontmatter
/// validator's own const, and an array literal here. They drift asymmetrically,
/// which is why this is the copy that had to go. Adding a seventh verb to the
/// gateway and the validator and missing this one would stop
/// `for_unreviewed_hosts` emitting a gate for it -- an un-approved request to every
/// unreviewed host, silently, which is the fail-open direction. The reverse miss is
/// a loud refusal at publish.
pub const METHODS: [&str; 6] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

const TAG_LEAF: &[u8] = b"outturn:gate:leaf:v1\0";
/// A grant's leaf, tagged apart from a gate's so the two can never be read as
/// each other. They share one tree because they answer one question -- what this
/// turn may do about an approval -- and a second claim would be a second thing
/// for a forger to strip independently.
const TAG_GRANT_LEAF: &[u8] = b"outturn:grant:leaf:v1\0";
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
    /// The request-body fields a person approving this is really approving, in
    /// the order the skill declared them.
    ///
    /// What a `call` grant is keyed on. Required wherever an approval is
    /// declared, because a grant keyed on less than what made the request
    /// distinctive is a grant that covers requests nobody looked at -- see
    /// `egress::grant`.
    #[serde(default)]
    pub binds: Vec<String>,
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
        let path = normalise_path(path);
        let pattern = normalise_path(&self.path);
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
/// The sentence a gated refusal ends with, and the mark that makes it ours.
///
/// Two tiers share it: `runtime::component` builds the refusal a guest is
/// answered with, and `api::worker::answered` finds it again to retract it once
/// somebody approves. A constant rather than the sentence written twice --
/// reword it in one place and the retraction silently becomes a no-op, and the
/// failure is the original bug back: the resumed turn reads "do not retry", says
/// nothing, and the grant goes unspent.
///
/// The mark is why this is not merely a shared string. A tool result is a remote
/// response kept verbatim, so a page the agent fetched can contain any sentence
/// the platform writes -- and a substring sweep would rewrite a refusal that was
/// never ours, or let a remote body pose as an approval the platform granted.
/// The marker is stripped before the reader ever sees it, and nothing a guest
/// fetches can emit it without the platform having put it there.
pub const GATED_MARK: &str = "\u{2060}outturn:gated\u{2060}";

/// What a gated refusal tells the guest to do, ending in `GATED_MARK`.
pub const GATED_REFUSAL: &str = "Do not retry this request.";

pub fn normalise_path(path: &str) -> String {
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

/// The act a host-level approval is about.
///
/// One word, as every `requires` is, and named for what a person is being asked
/// rather than for the setting that asked it: the queue row says "reach", and the
/// manager deciding does not need to know which switch produced the question.
pub const REACH: &str = "reach";

/// Gates for every host a turn may reach but has not had reviewed.
///
/// The `approve_new_hosts` setting, expressed as gates so that nothing downstream
/// has to know it exists. The commitment, the token claim, the gateway's check, the
/// refusal and the parked turn are all the per-operation machinery from
/// `docs/approvals.md`, and this reuses them whole.
///
/// A gate per host rather than one wildcard gate, and `*` for the path, because
/// the unit a person can honestly approve is a host: reaching `api.example.com`
/// twice is the same act both times, which is the test `docs/inhibitors.md` sets
/// and the reason the egress model can approve a host at all. Per request would
/// ask three times for three pages.
///
/// `exempt` is the hosts a skill's own declaration opened. Those were consented to
/// when the skill was installed, in an act that named the skill and the host
/// together; asking again per conversation is asking the same question somewhere
/// worse. A host added by hand through `/v1/egress-rules` is not exempt, which is
/// the case the setting exists for -- it says agents *may* reach it, not that any
/// use of it was reviewed.
///
/// Every method, because a read of an unreviewed host is as much a reach as a
/// write. The setting is about who the agent talks to rather than what it says.
pub fn for_unreviewed_hosts(allowed: &[String], exempt: &[String]) -> Vec<Gate> {
    allowed
        .iter()
        .filter(|host| !exempt.iter().any(|e| e == *host))
        .flat_map(|host| {
            METHODS.into_iter().map(move |method| Gate {
                requires: REACH.to_string(),
                host: host.clone(),
                method: method.to_string(),
                path: "/*".to_string(),
                identified_by: None,
                // Nothing from the body. What a person approves here is a host,
                // and reaching it twice is the same act both times -- which is
                // the test `docs/inhibitors.md` sets and the reason the egress
                // model can approve a host at all. A `reach` grant is therefore
                // keyed on the host and path alone, which is what an empty
                // `binds` digests to.
                binds: Vec::new(),
            })
        })
        .collect()
}

/// What a turn is gated by, as the gateway knows it.
///
/// Held as the whole set rather than proven one at a time, which is the opposite
/// of how egress rules travel and for the reason in the module comment: a
/// permission is proven by the request that wants it, and an obligation has to be
/// known in full before a request can be said not to match any of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gates {
    gates: Vec<Gate>,
    /// What somebody has already approved for this turn.
    ///
    /// Carried beside the gates rather than subtracted from them. A gate removed
    /// from the set is a gate nobody enforces, so an over-broad removal is an
    /// ungated request; a grant leaves the gate standing and permits one thing
    /// through it. See `api::grant::Granted`.
    #[serde(default)]
    grants: Vec<super::grant::Granted>,
}

impl Gates {
    /// A turn nothing gates.
    ///
    /// Distinct from a missing commitment, which is refused. This is the API
    /// saying "I looked and there are none", and it is signed.
    pub fn none() -> Self {
        Self {
            gates: Vec::new(),
            grants: Vec::new(),
        }
    }

    /// The same set, plus what has been approved for this turn.
    pub fn with_grants(mut self, mut grants: Vec<super::grant::Granted>) -> Self {
        // Sorted for the same reason the gates are: the commitment is over a
        // tree whose leaves must be built in one order on both sides.
        grants.sort_by(|a, b| {
            (&a.requires, a.extent.as_str(), &a.keyed_on).cmp(&(
                &b.requires,
                b.extent.as_str(),
                &b.keyed_on,
            ))
        });
        self.grants = grants;
        self
    }

    /// Whether any grant here permits this request through the gate that caught
    /// it.
    ///
    /// The gate is passed in because a unit grant is keyed on the field the
    /// *declaration* named, never on anything the caller chose.
    pub fn permitted(
        &self,
        gate: &Gate,
        method: &str,
        host: &str,
        path: &str,
        body: Option<&str>,
    ) -> Option<&super::grant::Granted> {
        self.grants
            .iter()
            .find(|g| g.permits(gate, method, host, path, body))
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
        Self {
            gates,
            grants: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.gates.is_empty()
    }

    /// The gates, for a caller adding to them before committing.
    ///
    /// Taken apart rather than extended in place so the sort in `of` cannot be
    /// skipped: a set that went out unsorted would decide which of two overlapping
    /// gates names a refusal by whatever order it was built in.
    pub fn into_vec(self) -> Vec<Gate> {
        self.gates
    }

    /// Every gate that covers this request, in the set's order.
    ///
    /// All of them, because each is an obligation and a request goes out only
    /// when every one is satisfied. Asking only the first was the bug: `/*` sorts
    /// before `/refunds`, so a grant for a host's `reach` gate -- approving
    /// "reach this host", once, for a `POST /refunds` -- let the refund out with
    /// its own gate never consulted.
    pub fn covering_all(&self, host: &str, method: &str, path: &str) -> Vec<&Gate> {
        self.gates
            .iter()
            .filter(|gate| gate.covers_request(host, method, path))
            .collect()
    }

    /// The first gate covering this request that nothing here has approved it
    /// through -- the one a refusal names and an approval asks about.
    ///
    /// `None` when no gate covers the request, or every one that does has a grant
    /// permitting it.
    pub fn unpermitted(
        &self,
        host: &str,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Option<&Gate> {
        self.covering_all(host, method, path)
            .into_iter()
            .find(|gate| self.permitted(gate, method, host, path, body).is_none())
    }

    /// The first gate that covers this request, if any. Whether a request is
    /// gated at all; never whether it may go out, which is every covering gate's
    /// question -- see `covering_all`.
    ///
    /// Two gates covering one request is not an error: a workspace binding two
    /// skills that both document the same endpoint is ordinary, and refusing would
    /// make a conversation impossible rather than gated.
    ///
    /// Sorted on construction so that choice is the same on every pod. It was not,
    /// and the order happened to be deterministic because `gates_for_turn` reads
    /// them ordered -- deterministic by accident of a query somebody may
    /// reasonably change, which is the same reason `leaves` sorts rather than
    /// assuming.
    pub fn covering(&self, host: &str, method: &str, path: &str) -> Option<&Gate> {
        self.gates
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
        // `commit::ordered`, shared: the dedup is what makes the carry-up tree
        // safe, and a copy of it here is a copy that can lose it.
        super::commit::ordered(
            self.gates
                .iter()
                .map(|g| leaf(workspace_id, g))
                .chain(self.grants.iter().map(|g| grant_leaf(workspace_id, g)))
                .collect(),
        )
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
        ]
        .into_iter()
        // The bound fields are part of what the gate is, because they decide
        // what a grant taken out under it covers. A runtime that could drop one
        // would widen every later grant to ignore that field.
        .chain(gate.binds.iter().map(|b| Some(b.as_str()))),
    );
    Hash(h.finalize().into())
}

/// A grant's leaf. Tagged apart from a gate's, and over every field that decides
/// what it permits -- a grant whose `keyed_on` could be changed without changing
/// the root would be a grant a runtime could point at a different booking.
fn grant_leaf(workspace_id: Uuid, granted: &super::grant::Granted) -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_GRANT_LEAF);
    h.update(workspace_id.as_bytes());
    super::commit::hash_fields(
        &mut h,
        [
            Some(granted.requires.as_str()),
            Some(granted.extent.as_str()),
            Some(granted.keyed_on.as_str()),
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
            binds: vec!["amount_pence".into()],
        }
    }

    fn workspace() -> Uuid {
        Uuid::from_bytes([7; 16])
    }

    /// A grant is part of what the token vouches for.
    ///
    /// The one thing standing between a compromised runtime and permission to
    /// send whatever it likes: it relays the gate set and the grants, so if the
    /// commitment did not cover them it could add a grant for the request it
    /// wants and the gateway would honour it. Left out of the tree, every other
    /// test in this crate still passed.
    #[test]
    fn a_grant_the_runtime_added_does_not_verify() {
        let workspace = workspace();
        let committed = Gates::of(vec![gate("charge", "POST", "/charges")]);
        let root = committed.root(workspace);

        let forged = Gates::of(vec![gate("charge", "POST", "/charges")]).with_grants(vec![
            super::super::grant::Granted {
                requires: "charge".into(),
                extent: super::super::grant::Extent::Call,
                keyed_on: "whatever this runtime wants".into(),
            },
        ]);

        assert!(
            !forged.matches(workspace, &root),
            "a grant nobody committed to must not verify"
        );
    }

    /// And one whose key was altered is a different grant.
    #[test]
    fn a_grant_cannot_be_repointed_after_it_is_committed() {
        let workspace = workspace();
        let granted = |keyed_on: &str| {
            Gates::of(vec![gate("charge", "POST", "/charges")]).with_grants(vec![
                super::super::grant::Granted {
                    requires: "charge".into(),
                    extent: super::super::grant::Extent::Call,
                    keyed_on: keyed_on.into(),
                },
            ])
        };

        let root = granted("the-approved-charge").root(workspace);
        assert!(
            !granted("some-other-charge").matches(workspace, &root),
            "changing what a grant is keyed on must break the commitment"
        );
    }

    /// Two turns' grants are not interchangeable, because the workspace is in
    /// every leaf.
    #[test]
    fn a_grant_does_not_verify_in_another_workspace() {
        let grants = vec![super::super::grant::Granted {
            requires: "charge".into(),
            extent: super::super::grant::Extent::Call,
            keyed_on: "a-charge".into(),
        }];
        let gates = Gates::of(vec![gate("charge", "POST", "/charges")]).with_grants(grants);

        let mine = workspace();
        let theirs = Uuid::now_v7();
        assert!(!gates.matches(theirs, &gates.root(mine)));
    }

    /// The bound fields are part of the gate, so a runtime cannot widen every
    /// later grant by dropping one.
    #[test]
    fn dropping_a_bound_field_breaks_the_commitment() {
        let workspace = workspace();
        let declared = Gates::of(vec![gate("charge", "POST", "/charges")]);
        let root = declared.root(workspace);

        let mut stripped = gate("charge", "POST", "/charges");
        stripped.binds.clear();
        assert!(
            !Gates::of(vec![stripped]).matches(workspace, &root),
            "a gate whose binds were dropped must not verify"
        );
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
            binds: Vec::new(),
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
            binds: Vec::new(),
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
            binds: Vec::new(),
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
            binds: Vec::new(),
        };
        for host in ["api.stripe.com.", "API.STRIPE.COM", "Api.Stripe.Com."] {
            assert!(
                g.covers_request(host, "POST", "/v1/charges"),
                "{host} evaded the gate"
            );
        }
    }
}

#[cfg(test)]
mod reaching {
    use super::*;

    #[test]
    fn every_allowed_host_is_gated_unless_a_skill_brought_it() {
        let gates = for_unreviewed_hosts(
            &[
                "api.stripe.com".to_string(),
                "outturn-hollowbrook".to_string(),
            ],
            &["outturn-hollowbrook".to_string()],
        );
        let set = Gates::of(gates);
        assert!(
            set.covering("api.stripe.com", "POST", "/v1/charges")
                .is_some(),
            "a host added by hand should be gated"
        );
        assert!(
            set.covering("outturn-hollowbrook", "POST", "/charges")
                .is_none(),
            "a host a skill brought was consented to when the skill was installed"
        );
    }

    #[test]
    fn a_host_nobody_allowed_is_not_gated_because_it_is_already_refused() {
        // Egress refuses it outright, and a gate would be a second answer to a
        // question already answered -- worse, one that reads as "approvable".
        let set = Gates::of(for_unreviewed_hosts(&["api.stripe.com".to_string()], &[]));
        assert!(set.covering("evil.test", "GET", "/").is_none());
    }

    #[test]
    fn every_method_is_gated() {
        // A read of an unreviewed host is as much a reach as a write: the setting
        // is about who the agent talks to, not what it says.
        let set = Gates::of(for_unreviewed_hosts(&["api.stripe.com".to_string()], &[]));
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"] {
            assert!(
                set.covering("api.stripe.com", method, "/anything")
                    .is_some(),
                "{method} was not gated"
            );
        }
    }

    #[test]
    fn any_path_on_a_gated_host_is_gated() {
        let set = Gates::of(for_unreviewed_hosts(&["api.stripe.com".to_string()], &[]));
        for path in ["/", "/v1/charges", "/deep/nested/thing", "//odd"] {
            assert!(
                set.covering("api.stripe.com", "GET", path).is_some(),
                "{path} was not gated"
            );
        }
    }

    #[test]
    fn a_wildcard_rule_gates_the_hosts_it_covers() {
        // An egress rule may be `*.example.com`, and the gate borrows its matcher.
        let set = Gates::of(for_unreviewed_hosts(&["*.example.com".to_string()], &[]));
        assert!(set.covering("api.example.com", "GET", "/").is_some());
        assert!(set.covering("notexample.com", "GET", "/").is_none());
    }

    #[test]
    fn exempting_everything_gates_nothing() {
        let hosts = vec!["api.stripe.com".to_string()];
        assert!(for_unreviewed_hosts(&hosts, &hosts).is_empty());
    }

    #[test]
    fn these_gates_commit_like_any_other() {
        // The point of expressing the setting this way: nothing downstream knows
        // it is different.
        let workspace = uuid::Uuid::from_bytes([5; 16]);
        let set = Gates::of(for_unreviewed_hosts(&["api.stripe.com".to_string()], &[]));
        let root = set.root(workspace);
        assert!(set.matches(workspace, &root));
        assert!(!Gates::none().matches(workspace, &root));
    }
}

#[cfg(test)]
mod the_tree_shape {
    use super::*;

    fn gate(requires: &str) -> Gate {
        Gate {
            requires: requires.into(),
            host: "api.example.com".into(),
            method: "POST".into(),
            path: "/charges".into(),
            identified_by: None,
            binds: Vec::new(),
        }
    }

    /// The dedup, asserted on this side too.
    ///
    /// `commit.rs` has had this test since it was written; this side shared the
    /// tree and the ordering but had no test of its own, which made the copy that
    /// matters most the untested one. Under the carry-up shape `[a, b, c]` and
    /// `[a, b, c, c]` hash alike without dedup, so one commitment vouches for two
    /// sets -- and here the second set can be the *smaller* one, which is a gate
    /// vanishing and a charge going out unapproved.
    #[test]
    fn a_repeat_does_not_make_a_second_set_the_root_vouches_for() {
        let workspace = uuid::Uuid::from_bytes([11; 16]);
        let a = gate("charge");
        let b = gate("refund");
        let c = gate("comp");

        // Deduplication is deliberate: two bound skills documenting one endpoint
        // is ordinary, so these two are the same set and share a root.
        assert_eq!(
            Gates::of(vec![a.clone(), b.clone(), c.clone()])
                .root(workspace)
                .0,
            Gates::of(vec![a.clone(), b.clone(), c.clone(), c.clone()])
                .root(workspace)
                .0,
        );

        // What must not follow from that: a genuinely smaller set passing.
        let all = Gates::of(vec![a.clone(), b.clone(), c]);
        let committed = all.root(workspace);
        assert!(!Gates::of(vec![a, b]).matches(workspace, &committed));
    }
}
