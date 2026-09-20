//! What the API promises about a workspace's egress rules, and how the tier
//! that enforces them checks that promise.
//!
//! The runtime runs workspace code, so it cannot be the tier that decides what
//! a workspace allowed -- it would be deciding about itself. But it is the tier
//! holding the rules, because they travel with the turn rather than being read
//! where they are used. So the API commits to the rule set when it mints the
//! turn, the runtime carries the commitment it was given, and whoever enforces
//! a rule checks that the rule it was handed is one the API actually vouched
//! for. A rule the runtime invented hashes to something else and is refused.
//!
//! The commitment is one hash whatever the workspace allows, so the turn token
//! does not grow with the rule list. What varies is the proof carried beside a
//! request: the whole rule set for a short list, or a single rule and its
//! inclusion proof once that is the cheaper of the two. Both are the same
//! verification -- a whole-set proof is the degenerate case where the path is
//! every other leaf -- so there is one verifier rather than two that must agree.
//!
//! What this does *not* do is prove absence. An inclusion proof says a rule is
//! in the set; nothing here can show a host is missing from it. That is fine in
//! one direction only: denial is the default, so a host is refused by failing
//! to prove it was allowed, and no proof of absence is ever needed. Anything
//! built on this must keep that direction -- "could not verify" has to mean
//! refused, never allowed.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::runtime::egress::EgressRule;

/// A 32-byte SHA-256 output: a leaf, a node, or the root the token carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Hash(#[serde(with = "hex_bytes")] pub [u8; 32]);

impl Hash {
    /// Compared in constant time. These are public values and an attacker
    /// learning one byte at a time gains little, but a comparison that returns
    /// early on the first difference is a habit worth not having in the tier
    /// that decides whether a request goes out.
    pub fn matches(&self, other: &Hash) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl std::fmt::Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

/// Hex in JSON, bytes in memory: a commitment travels in a token claim and in
/// request bodies, and 64 characters of hex survive both without a base64
/// alphabet question.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let mut out = [0u8; 32];
        hex::decode_to_slice(&text, &mut out)
            .map_err(|_| serde::de::Error::custom("not 32 bytes of hex"))?;
        Ok(out)
    }
}

/// Domain separation. Every hash here is taken over one of these tags followed
/// by its input, so a value that is a leaf in one position can never be read as
/// a node in another -- the attack where a crafted "rule" is fed in as an
/// interior hash and the tree is rebuilt around it.
const TAG_LEAF: &[u8] = b"outturn:egress:leaf:v1\0";
const TAG_NODE: &[u8] = b"outturn:egress:node:v1\0";
const TAG_EMPTY: &[u8] = b"outturn:egress:empty:v1\0";

/// Why a proof was not accepted.
///
/// Every variant means the same thing to a caller -- do not make this request --
/// and they are separate only so a log line can say which way it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// The rebuilt root is not the one the API committed to. A rule the runtime
    /// altered or invented lands here.
    Root,
    /// The path is longer than any tree of this size could need, so verifying
    /// it would be work chosen by the caller rather than by the data.
    PathTooLong,
    /// A whole-set proof carrying no rules, offered against a commitment that
    /// is not the empty one.
    Empty,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Invalid::Root => write!(f, "these rules are not the ones this turn was given"),
            Invalid::PathTooLong => write!(f, "that inclusion proof is implausibly long"),
            Invalid::Empty => write!(f, "no rules were offered for a turn that has some"),
        }
    }
}

impl std::error::Error for Invalid {}

/// The most leaves a path may address: 2^32 rules is far past anything real,
/// and the bound is what stops a caller spending the verifier's time.
const MAX_PATH: usize = 32;

/// One rule's leaf hash.
///
/// Bound to the workspace, so a proof from one workspace cannot be replayed
/// against another's commitment, and to every field of the rule rather than the
/// host alone: the header and the credential decide what travels with the
/// request, and a rule whose credential could be swapped for another host's is
/// a rule that leaks it.
///
/// Fields are length-prefixed rather than delimited. `host=a\0header=bc` and
/// `host=ab\0header=c` are different rules and must not hash alike, which is
/// exactly what a delimiter cannot promise once a field may contain it.
fn leaf(workspace_id: Uuid, rule: &EgressRule) -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_LEAF);
    h.update(workspace_id.as_bytes());
    for field in [
        Some(rule.host.as_str()),
        rule.header.as_deref(),
        rule.credential_env.as_deref(),
    ] {
        // A present-but-empty field and an absent one are different rules.
        match field {
            Some(value) => {
                h.update([1u8]);
                h.update((value.len() as u64).to_le_bytes());
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

/// The commitment for a workspace that allows nothing.
///
/// Given its own tag rather than being left as a missing claim or the hash of
/// nothing. A token whose commitment was stripped must not read as "this
/// workspace allows nothing" -- that is precisely the answer an attacker would
/// choose, and the one the rest of the system treats as safe. Every turn
/// carries a commitment, and this is what it says when the list is empty.
pub fn empty_root() -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_EMPTY);
    Hash(h.finalize().into())
}

/// Rules in the order the tree is built over, which is sorted and deduplicated.
///
/// The API and the runtime must agree byte for byte on this, and the database
/// already returns rules ordered by host -- but "already ordered" is a property
/// of a query somebody may reasonably change, so the order is taken here rather
/// than assumed. Sorting by the whole leaf rather than by host keeps it
/// deterministic even if a host ever appears twice.
fn leaves(workspace_id: Uuid, rules: &[EgressRule]) -> Vec<Hash> {
    let mut leaves: Vec<Hash> = rules.iter().map(|r| leaf(workspace_id, r)).collect();
    leaves.sort_unstable_by_key(|h| h.0);
    leaves.dedup_by(|a, b| a.0 == b.0);
    leaves
}

/// The root the API commits to, over every rule a turn is given.
pub fn root(workspace_id: Uuid, rules: &[EgressRule]) -> Hash {
    root_of(&leaves(workspace_id, rules))
}

/// A tree that carries an odd level up rather than duplicating its last node.
///
/// Duplicating is the well-known shape and the well-known bug with it: a
/// duplicated leaf lets a different rule set hash to the same root, which is a
/// second set of rules the same commitment vouches for.
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

/// One step up the tree: a sibling, and which side it is on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub hash: Hash,
    /// True when the sibling is the left of the pair, so the verifier combines
    /// them in the order the API did.
    pub left: bool,
}

/// What a request carries to show its rule was one the API vouched for.
///
/// Two shapes, one meaning. A workspace with a handful of rules sends them all
/// and spends nothing thinking about it; one with hundreds sends a rule and a
/// path that grows with the logarithm of the list. The verifier treats them the
/// same way, so neither can be the lenient one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Proof {
    /// Every rule the turn was given.
    WholeSet { rules: Vec<EgressRule> },
    /// One rule and its path to the root.
    Inclusion { rule: EgressRule, path: Vec<Step> },
}

impl Proof {
    /// The rule this proof is about, before it has been shown to be genuine.
    ///
    /// Named so that using it without verifying reads oddly. For a whole-set
    /// proof the caller still has to find its host among the rules, which is
    /// the existing matching in `runtime::egress`, so this answers only for the
    /// shape that names one.
    pub fn unverified_rule(&self) -> Option<&EgressRule> {
        match self {
            Proof::WholeSet { .. } => None,
            Proof::Inclusion { rule, .. } => Some(rule),
        }
    }
}

/// Builds the smaller of the two proofs for one rule.
///
/// The whole set is smaller until the list is longer than a path would be, so
/// small workspaces pay nothing for a mechanism that exists for large ones.
/// Returns `None` when the rule is not in the set, which is a caller asking to
/// prove something untrue rather than an error to carry anywhere.
pub fn prove(workspace_id: Uuid, rules: &[EgressRule], rule: &EgressRule) -> Option<Proof> {
    let leaves = leaves(workspace_id, rules);
    let target = leaf(workspace_id, rule);
    let index = leaves.iter().position(|l| *l == target)?;

    let mut path = Vec::new();
    let mut level = leaves.clone();
    let mut index = index;
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut pairs = level.chunks_exact(2);
        for pair in &mut pairs {
            next.push(node(&pair[0], &pair[1]));
        }
        if let [odd] = pairs.remainder() {
            next.push(*odd);
        }
        // The odd one out has no sibling at this level: it is carried up
        // untouched, so there is no step to record for it.
        if index + 1 < level.len() || index % 2 == 1 {
            let sibling = if index % 2 == 0 { index + 1 } else { index - 1 };
            path.push(Step {
                hash: level[sibling],
                left: index % 2 == 1,
            });
        }
        index /= 2;
        level = next;
    }

    // Whichever is smaller on the wire, measured rather than estimated. A step
    // is a fixed 32 bytes and a rule is three strings of nobody's guess, and
    // both shapes carry their own JSON framing -- an estimate that ignored it
    // sends a path for a single rule, where there is no sibling to name and the
    // rule itself is the shorter thing to say.
    let inclusion = Proof::Inclusion {
        rule: rule.clone(),
        path,
    };
    let whole = Proof::WholeSet {
        rules: rules.to_vec(),
    };
    match (encoded_len(&inclusion), encoded_len(&whole)) {
        (Some(a), Some(b)) if a <= b => Some(inclusion),
        (Some(_), Some(_)) => Some(whole),
        // A rule set that will not serialise is one nothing can send anyway;
        // the inclusion proof is the bounded shape, so it is the safer default.
        _ => Some(inclusion),
    }
}

/// How long a proof is once encoded, which is the only honest way to compare
/// two shapes that frame themselves differently.
fn encoded_len(proof: &Proof) -> Option<usize> {
    serde_json::to_vec(proof).ok().map(|v| v.len())
}

/// Checks a proof against the commitment the turn carries, and answers with the
/// rules it vouches for.
///
/// Every caller is deciding whether a request goes out, so this returns rules
/// rather than a boolean: there is then no way to check a proof and go on to
/// use a rule that was not the one checked.
///
/// The workspace binds one of the two shapes and not the other, which is worth
/// being plain about. An inclusion proof carries the workspace in every leaf,
/// so it verifies under one workspace and no other. A whole-set proof is
/// rehashed under whichever workspace is passed in, so it proves only "these
/// rules hash to this commitment" -- leaving the commitment as the one thing an
/// attacker must not be able to choose. That holds here because it comes out of
/// the turn token, which the API signed for one workspace. A caller taking a
/// commitment from anywhere else has to bind it itself.
pub fn verify(
    workspace_id: Uuid,
    committed: &Hash,
    proof: &Proof,
) -> Result<Vec<EgressRule>, Invalid> {
    match proof {
        Proof::WholeSet { rules } => {
            // An empty set is legitimate, but only against the commitment that
            // says so -- otherwise "I was given no rules" would be a way to
            // turn a turn that has some into one that has none.
            if rules.is_empty() && !committed.matches(&empty_root()) {
                return Err(Invalid::Empty);
            }
            if !root(workspace_id, rules).matches(committed) {
                return Err(Invalid::Root);
            }
            Ok(rules.clone())
        }
        Proof::Inclusion { rule, path } => {
            if path.len() > MAX_PATH {
                return Err(Invalid::PathTooLong);
            }
            let mut hash = leaf(workspace_id, rule);
            for step in path {
                hash = if step.left {
                    node(&step.hash, &hash)
                } else {
                    node(&hash, &step.hash)
                };
            }
            if !hash.matches(committed) {
                return Err(Invalid::Root);
            }
            Ok(vec![rule.clone()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(host: &str) -> EgressRule {
        EgressRule {
            host: host.into(),
            header: None,
            credential_env: None,
        }
    }

    fn with_credential(host: &str, header: &str, env: &str) -> EgressRule {
        EgressRule {
            host: host.into(),
            header: Some(header.into()),
            credential_env: Some(env.into()),
        }
    }

    fn rules(hosts: &[&str]) -> Vec<EgressRule> {
        hosts.iter().map(|h| rule(h)).collect()
    }

    /// Every size up to a few levels, because the shapes that break a Merkle
    /// tree are the odd ones: a single leaf, an odd level carried up, the level
    /// above that being odd in turn.
    fn sizes() -> Vec<usize> {
        (1..=17).collect()
    }

    fn many(n: usize) -> Vec<EgressRule> {
        (0..n)
            .map(|i| rule(&format!("host{i}.example.com")))
            .collect()
    }

    #[test]
    fn a_rule_the_api_vouched_for_verifies() {
        let ws = Uuid::now_v7();
        for n in sizes() {
            let set = many(n);
            let committed = root(ws, &set);
            for r in &set {
                let proof = prove(ws, &set, r).expect("in the set");
                let vouched = verify(ws, &committed, &proof).expect("verifies");
                assert!(vouched.contains(r), "n={n} lost {}", r.host);
            }
        }
    }

    #[test]
    fn a_rule_the_runtime_invented_does_not() {
        let ws = Uuid::now_v7();
        let set = rules(&["api.example.com", "b.example.com", "c.example.com"]);
        let committed = root(ws, &set);

        let forged = Proof::Inclusion {
            rule: rule("evil.example.com"),
            path: Vec::new(),
        };
        assert_eq!(verify(ws, &committed, &forged), Err(Invalid::Root));

        let mut tampered = set.clone();
        tampered.push(rule("evil.example.com"));
        let whole = Proof::WholeSet { rules: tampered };
        assert_eq!(verify(ws, &committed, &whole), Err(Invalid::Root));
    }

    #[test]
    fn a_rule_cannot_be_swapped_for_another_hosts_credential() {
        let ws = Uuid::now_v7();
        let set = vec![
            with_credential("api.stripe.com", "authorization", "STRIPE_KEY"),
            rule("docs.example.com"),
        ];
        let committed = root(ws, &set);

        // The host that was allowed, carrying the credential of the one that
        // pays. Nothing about the rule may be editable in flight.
        let swapped = Proof::Inclusion {
            rule: with_credential("docs.example.com", "authorization", "STRIPE_KEY"),
            path: vec![Step {
                hash: leaf(ws, &set[0]),
                left: true,
            }],
        };
        assert_eq!(verify(ws, &committed, &swapped), Err(Invalid::Root));
    }

    #[test]
    fn an_inclusion_proof_does_not_travel_between_workspaces() {
        let mine = Uuid::now_v7();
        let yours = Uuid::now_v7();
        // Long enough that the cheaper proof is the path, which is the shape
        // that carries the workspace in its leaf.
        let set = many(40);
        let proof = prove(mine, &set, &set[0]).expect("in the set");
        assert!(matches!(proof, Proof::Inclusion { .. }));

        assert!(verify(mine, &root(mine, &set), &proof).is_ok());
        // The same rules, allowed by somebody else: every leaf differs, so the
        // path rebuilds to a root that is nobody's commitment.
        assert_eq!(
            verify(yours, &root(yours, &set), &proof),
            Err(Invalid::Root)
        );
        assert_eq!(verify(yours, &root(mine, &set), &proof), Err(Invalid::Root));
    }

    #[test]
    fn a_whole_set_proof_is_bound_by_its_commitment_alone() {
        // Documented rather than defended: a whole-set proof is rehashed under
        // whichever workspace it is checked for, so it says "these rules hash
        // to this commitment" and nothing about whose rules they are. What
        // makes that safe is where the commitment comes from -- the turn token,
        // signed by the API for one workspace -- so this is a property callers
        // must not undo by taking a commitment from somewhere else.
        let mine = Uuid::now_v7();
        let yours = Uuid::now_v7();
        let set = rules(&["api.example.com", "b.example.com"]);

        let proof = prove(mine, &set, &set[0]).expect("in the set");
        assert!(matches!(proof, Proof::WholeSet { .. }));
        assert!(verify(yours, &root(yours, &set), &proof).is_ok());
        // Against the commitment it was actually made under, another
        // workspace's check still fails.
        assert_eq!(verify(yours, &root(mine, &set), &proof), Err(Invalid::Root));
    }

    #[test]
    fn allowing_nothing_is_something_the_api_says() {
        let ws = Uuid::now_v7();
        let committed = root(ws, &[]);
        assert!(committed.matches(&empty_root()));

        // The shape an attacker would choose: a turn that was given rules,
        // presented as one that was given none.
        let stripped = Proof::WholeSet { rules: Vec::new() };
        assert_eq!(
            verify(ws, &root(ws, &rules(&["api.example.com"])), &stripped),
            Err(Invalid::Empty)
        );
        // And the honest case still works.
        assert_eq!(verify(ws, &committed, &stripped), Ok(Vec::new()));
    }

    #[test]
    fn the_empty_commitment_is_not_the_hash_of_nothing() {
        // A tag of its own, so it cannot be produced by hashing an empty input
        // somewhere else in the system and offered as "this workspace allows
        // nothing".
        assert_ne!(empty_root().0, <[u8; 32]>::from(Sha256::digest([])));
        assert_ne!(
            empty_root(),
            root(Uuid::now_v7(), &rules(&["a.example.com"]))
        );
    }

    #[test]
    fn a_leaf_cannot_be_passed_off_as_a_node() {
        let ws = Uuid::now_v7();
        let set = rules(&["a.example.com", "b.example.com"]);
        let committed = root(ws, &set);

        // The interior hash offered as a leaf: without separate tags for leaves
        // and nodes, this is the classic second preimage.
        let interior = EgressRule {
            host: hex::encode(committed.0),
            header: None,
            credential_env: None,
        };
        let forged = Proof::Inclusion {
            rule: interior,
            path: Vec::new(),
        };
        assert_eq!(verify(ws, &committed, &forged), Err(Invalid::Root));
    }

    #[test]
    fn fields_cannot_be_shuffled_between_each_other() {
        let ws = Uuid::now_v7();
        // Same bytes, different boundaries: a delimiter would hash these alike.
        let a = with_credential("a.example.com", "xy", "Z");
        let b = with_credential("a.example.com", "x", "yZ");
        assert_ne!(leaf(ws, &a), leaf(ws, &b));

        // And absent is not the same as present-but-empty.
        let absent = rule("a.example.com");
        let empty = with_credential("a.example.com", "", "");
        assert_ne!(leaf(ws, &absent), leaf(ws, &empty));
    }

    #[test]
    fn a_duplicated_leaf_does_not_forge_a_second_set() {
        // The bug in the usual odd-level shape: with duplication, [a, b, c] and
        // [a, b, c, c] share a root, so one commitment vouches for two sets.
        let ws = Uuid::now_v7();
        let three = rules(&["a.example.com", "b.example.com", "c.example.com"]);
        let mut four = three.clone();
        four.push(three[2].clone());
        // Deduplication makes them the same set here, which is the point: the
        // extra copy cannot smuggle in a rule.
        assert_eq!(root(ws, &three), root(ws, &four));

        let mut different = three.clone();
        different.push(rule("d.example.com"));
        assert_ne!(root(ws, &three), root(ws, &different));
    }

    #[test]
    fn the_order_rules_arrive_in_does_not_matter() {
        let ws = Uuid::now_v7();
        let forwards = rules(&["a.example.com", "b.example.com", "c.example.com"]);
        let backwards: Vec<_> = forwards.iter().rev().cloned().collect();
        assert_eq!(root(ws, &forwards), root(ws, &backwards));

        // And a proof built from one order verifies against the other.
        let proof = prove(ws, &backwards, &forwards[0]).expect("in the set");
        assert!(verify(ws, &root(ws, &forwards), &proof).is_ok());
    }

    #[test]
    fn a_path_longer_than_any_tree_is_refused_before_it_is_walked() {
        let ws = Uuid::now_v7();
        let set = rules(&["a.example.com"]);
        let padded = Proof::Inclusion {
            rule: set[0].clone(),
            path: (0..MAX_PATH + 1)
                .map(|_| Step {
                    hash: empty_root(),
                    left: false,
                })
                .collect(),
        };
        assert_eq!(
            verify(ws, &root(ws, &set), &padded),
            Err(Invalid::PathTooLong)
        );
    }

    #[test]
    fn the_cheaper_proof_is_the_one_that_travels() {
        let ws = Uuid::now_v7();
        // Whichever shape is chosen, it is the smaller of the two on the wire.
        // Asserted as the property rather than as a shape per size: where the
        // crossover falls depends on how long the hostnames are, and pinning it
        // would be a test of this test's fixtures.
        for n in [1usize, 2, 3, 8, 40, 100] {
            let set = many(n);
            let proof = prove(ws, &set, &set[0]).expect("in the set");
            let whole = Proof::WholeSet { rules: set.clone() };
            let chosen = serde_json::to_string(&proof).expect("serialises").len();
            let alternative = serde_json::to_string(&whole).expect("serialises").len();
            assert!(
                chosen <= alternative,
                "n={n}: sent {chosen} bytes where {alternative} would have done"
            );
            assert!(verify(ws, &root(ws, &set), &proof).is_ok(), "n={n}");
        }

        // And a list long enough to matter travels as a path, which is the
        // whole reason the tree is here.
        let large = many(100);
        match prove(ws, &large, &large[0]).expect("in the set") {
            Proof::Inclusion { path, .. } => assert!(path.len() <= 7, "path was {}", path.len()),
            Proof::WholeSet { .. } => panic!("a hundred rules should not travel whole"),
        }
    }

    #[test]
    fn proving_a_rule_that_was_never_allowed_is_not_possible() {
        let ws = Uuid::now_v7();
        let set = rules(&["a.example.com"]);
        assert_eq!(prove(ws, &set, &rule("b.example.com")), None);
    }

    #[test]
    fn a_commitment_survives_a_round_trip_through_json() {
        let ws = Uuid::now_v7();
        let set = many(9);
        let committed = root(ws, &set);
        let proof = prove(ws, &set, &set[4]).expect("in the set");

        let wire = serde_json::to_string(&(committed, &proof)).expect("serialises");
        let (there, back): (Hash, Proof) = serde_json::from_str(&wire).expect("deserialises");

        assert_eq!(there, committed);
        assert!(verify(ws, &there, &back).is_ok());
        // Hex rather than an array of numbers, so a claim stays one short string.
        assert!(wire.contains(&hex::encode(committed.0)));
    }
}
