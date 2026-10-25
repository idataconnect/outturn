//! A yes given in advance, as the tier making the request checks it.
//!
//! See `docs/auto-approval.md`. A workspace may decide that some gated requests
//! no longer need a person: an `approval_policies` row, set by somebody holding
//! `approvals:delegate` and `approvals:answer`. The API resolves which policy
//! applies to each of a turn's gates when it prepares the turn and commits what
//! survives into the turn token beside the gates and grants (`egress::gate`), so
//! the gateway decides from the token, like every other grant.
//!
//! Beside `grant.rs` because both tiers read this. The API builds a
//! `PolicyGrant` and commits to it; the gateway checks a live request against it
//! and writes the record (`record`) before the request goes out. The language a
//! policy is written in is the small one below -- a conjunction of tests on
//! fields the gate already binds -- because a policy is what decides whether
//! money moves without a person, and what it permits should be readable off the
//! row.
//!
//! Every test here fails closed. A comparison on a string that looks like a
//! number, a float against an integer bound, a field the request does not carry:
//! each is a test that does not hold, and a request a policy does not admit asks
//! a person, exactly as it would have with no policy at all.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::commit::Hash;
use super::gate::Gate;

const TAG_POLICY_LEAF: &[u8] = b"outturn:policy:leaf:v1\0";

/// How many tests one policy may carry. A policy long enough to need more is
/// one nobody can read off the row, which is the property the language exists
/// to keep.
pub const MAX_TESTS: usize = 32;

/// What an operation's declaration says about how much is at stake.
///
/// A selector and nothing more: it grants nothing by itself, and nothing but a
/// policy choosing what it covers reads it. Ordered, so "medium or lower" is a
/// comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "low" => Some(Risk::Low),
            "medium" => Some(Risk::Medium),
            "high" => Some(Risk::High),
            _ => None,
        }
    }
}

/// One test's comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    In,
    NotIn,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    /// Whether this orders values rather than matching them, and so may only be
    /// applied to a field the declaration lists in `numbers`.
    pub fn compares(self) -> bool {
        matches!(self, Op::Lt | Op::Le | Op::Gt | Op::Ge)
    }

    /// Whether this names the values it admits, which is what `≠` and "not one
    /// of" must stand beside. On their own they admit every value nobody thought
    /// of.
    fn positive(self) -> bool {
        matches!(self, Op::Eq | Op::In)
    }

    fn excludes(self) -> bool {
        matches!(self, Op::Ne | Op::NotIn)
    }
}

/// One test on one bound field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Test {
    pub field: String,
    pub op: Op,
    /// A scalar for `eq` and `ne`, a list of scalars for `in` and `not_in`, an
    /// integer for a comparison.
    pub value: serde_json::Value,
}

/// Whether a set of tests is one this language can say, before anything asks
/// what it admits.
///
/// Checked when a policy is written, so a row that reached the database is one
/// whose meaning is the one below.
pub fn check_tests(tests: &[Test]) -> Result<(), String> {
    if tests.len() > MAX_TESTS {
        return Err(format!("a policy may carry at most {MAX_TESTS} tests"));
    }
    for test in tests {
        if test.field.trim().is_empty() {
            return Err("a test needs a field".into());
        }
        match test.op {
            Op::Eq | Op::Ne => {
                if !scalar(&test.value) {
                    return Err(format!(
                        "{} is compared with a single text, whole number or true/false value",
                        test.field
                    ));
                }
            }
            Op::In | Op::NotIn => {
                let list = test.value.as_array().filter(|l| !l.is_empty());
                if !list.is_some_and(|l| l.iter().all(scalar)) {
                    return Err(format!(
                        "{} is compared with a list of text, whole number or true/false values",
                        test.field
                    ));
                }
            }
            Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                if test.value.as_i64().is_none() {
                    return Err(format!("{} is bounded by a whole number", test.field));
                }
            }
        }
        if test.op.excludes()
            && !tests
                .iter()
                .any(|t| t.field == test.field && t.op.positive())
        {
            return Err(format!(
                "{} may be excluded only beside a test naming the values it may take",
                test.field
            ));
        }
    }
    Ok(())
}

/// A value a test may name: text, a whole number in the 64-bit range, or true or
/// false. Never a float, which would compare by a rounding nobody chose.
fn scalar(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(_) | serde_json::Value::Bool(_) => true,
        serde_json::Value::Number(n) => n.as_i64().is_some(),
        _ => false,
    }
}

/// Whether a policy's tests can be applied to a gate at all.
///
/// A policy covers a gate only if the gate binds every field it tests and lists
/// in `numbers` every field it compares or sums. A risk or act selector spans
/// gates binding different fields, and one that does not bind `amount_pence` is
/// not covered by a policy testing it -- rather than covered with the test
/// skipped, which would be the policy admitting more than it says.
pub fn fits(tests: &[Test], sum_field: Option<&str>, binds: &[String], numbers: &[String]) -> bool {
    let bound = |f: &str| binds.iter().any(|b| b == f);
    let numeric = |f: &str| numbers.iter().any(|n| n == f);
    tests
        .iter()
        .all(|t| bound(&t.field) && (!t.op.compares() || numeric(&t.field)))
        && sum_field.is_none_or(|f| bound(f) && numeric(f))
}

/// Two values the same, by the rules a test applies: same kind and same value,
/// and numbers only as whole numbers in range.
fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value::*;
    match (a, b) {
        (String(x), String(y)) => x == y,
        (Bool(x), Bool(y)) => x == y,
        (Number(x), Number(y)) => x.as_i64().is_some() && x.as_i64() == y.as_i64(),
        _ => false,
    }
}

/// Whether every test holds of a request body's top-level fields.
///
/// Absent never matches -- not zero, not the empty string, not null -- and a
/// field any comparison bounds is bounded below at zero unless a test says
/// otherwise, because `≤ 5000` alone admits `-500000`, which is a refund dressed
/// as a small charge.
pub fn holds(tests: &[Test], body: &serde_json::Map<String, serde_json::Value>) -> bool {
    let each = tests.iter().all(|test| {
        let Some(value) = body.get(&test.field).filter(|v| !v.is_null()) else {
            return false;
        };
        match test.op {
            Op::Eq => same(value, &test.value),
            Op::Ne => scalar(value) && !same(value, &test.value),
            Op::In => list(&test.value).iter().any(|v| same(value, v)),
            Op::NotIn => scalar(value) && !list(&test.value).iter().any(|v| same(value, v)),
            Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                // A JSON number and a whole one, compared exactly. A string that
                // looks like one, a float, or anything outside the 64-bit range
                // is a test that fails.
                let (Some(v), Some(bound)) = (whole(value), test.value.as_i64()) else {
                    return false;
                };
                match test.op {
                    Op::Lt => v < bound,
                    Op::Le => v <= bound,
                    Op::Gt => v > bound,
                    _ => v >= bound,
                }
            }
        }
    });
    if !each {
        return false;
    }
    // The implicit floor, for every compared field nobody gave a lower bound.
    tests
        .iter()
        .filter(|t| t.op.compares())
        .filter(|t| {
            !tests
                .iter()
                .any(|o| o.field == t.field && matches!(o.op, Op::Gt | Op::Ge))
        })
        .all(|t| body.get(&t.field).and_then(whole).is_some_and(|v| v >= 0))
}

fn list(value: &serde_json::Value) -> &[serde_json::Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// A JSON number that is a whole one in range, and nothing else.
fn whole(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// A policy as it travels in a turn's commitment, about one gate.
///
/// Everything the gateway needs to decide, and nothing it would have to look up:
/// the gate it is about (by that gate's own leaf, so a policy for one skill's
/// `charge` says nothing about another's), the tests, which top-level fields a
/// request may carry at all, and the caps. Committed with its own tagged leaf, so
/// a runtime that loosened a test or raised a cap breaks the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyGrant {
    pub policy_id: Uuid,
    /// The row's version when the turn was prepared. The record is written only
    /// against the same version, so an edit reaches a running turn as a person
    /// being asked rather than as the old rule carrying on.
    pub version: i32,
    /// The turn this was resolved for, which is what the per-turn cap counts.
    pub job_id: Uuid,
    /// `Gate::key` of the gate this is about.
    pub gate: String,
    pub requires: String,
    pub tests: Vec<Test>,
    /// The top-level body fields a request may carry: the gate's `binds` and its
    /// declared `free`, sorted. A field in neither is one nobody has said is
    /// harmless, and a request carrying it is not admitted.
    pub fields: Vec<String>,
    pub per_turn: i64,
    pub per_day: i64,
    pub sum_field: Option<String>,
    pub sum_per_day: Option<i64>,
    /// The bands the gate's operation runs under in this turn, where it
    /// declares an amount. Absent from a gate that does not, whose risk is its
    /// base risk and was settled when the turn was prepared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banding: Option<super::amount::Banding>,
    /// The highest risk a request worked out from `banding` may have and still
    /// be admitted: the tighter of the policy's selector and the operator's
    /// ceiling. Read only beside `banding`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_risk: Option<Risk>,
    /// The most of `banding`'s currency this policy lets out in a day, across
    /// everything it covers. A request whose amount cannot be read is not
    /// admitted against one, since it could not be counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency_per_day: Option<i64>,
}

/// What a policy admitted, and what the gateway sends and records for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Admitted {
    /// The bound fields' values, which is what the policy tested and what a
    /// person reading the record later needs. Not the body.
    pub bound: serde_json::Value,
    /// The summed field's value, where the policy sums one.
    pub amount: Option<i64>,
    /// The amount in the operation's declared currency, where it declares one
    /// and the request carried one to trust: what a daily total counts, in the
    /// currency's standard units (`amount::to_standard`).
    pub money: Option<(String, i64)>,
    /// The body as it was parsed, serialized again. What goes out, so what was
    /// tested is what is sent: a body naming a field twice cannot satisfy the
    /// policy with one copy and reach the remote API with the other.
    pub body: String,
}

impl PolicyGrant {
    /// Whether this policy admits the request in hand, through the gate it is
    /// about.
    ///
    /// `None` is "ask a person", never an error: a query string on a gated path
    /// (which no test saw), a body that is not one JSON object, a top-level field
    /// outside `fields` or differing from a bound one only by case, or a test
    /// that does not hold.
    pub fn admits(
        &self,
        workspace_id: Uuid,
        gate: &Gate,
        query: Option<&str>,
        body: Option<&str>,
    ) -> Option<Admitted> {
        if self.gate != gate.key(workspace_id) || self.requires != gate.requires {
            return None;
        }
        // A gate that binds nothing is a host gate, which no policy covers: see
        // the doc's "Host gates".
        if gate.binds.is_empty() || query.is_some() {
            return None;
        }
        let serde_json::Value::Object(parsed) = serde_json::from_str(body?).ok()? else {
            return None;
        };
        for key in parsed.keys() {
            if !self.fields.iter().any(|f| f == key) {
                return None;
            }
            // `Amount_Pence` beside `amount_pence`: two spellings a remote API may
            // read as one.
            if gate
                .binds
                .iter()
                .any(|b| b != key && b.eq_ignore_ascii_case(key))
            {
                return None;
            }
        }
        if !holds(&self.tests, &parsed) {
            return None;
        }
        let amount = match &self.sum_field {
            Some(field) => Some(parsed.get(field).and_then(whole).filter(|v| *v >= 0)?),
            None => None,
        };
        // The request's own risk, from the bands the token commits rather than
        // anything the request says about itself.
        let money = match &self.banding {
            Some(banding) => {
                let (risk, money) = banding.assess(&parsed);
                if self.max_risk.is_none_or(|max| risk > max) {
                    return None;
                }
                // In the currency's standard units, as the day's total is.
                money
                    .and_then(|m| banding.counted(m))
                    .map(|m| (banding.currency.clone(), m))
            }
            None => None,
        };
        if self.currency_per_day.is_some() && money.is_none() {
            return None;
        }
        let bound = serde_json::Value::Object(
            gate.binds
                .iter()
                .filter_map(|b| parsed.get(b).map(|v| (b.clone(), v.clone())))
                .collect(),
        );
        Some(Admitted {
            bound,
            amount,
            money,
            body: serde_json::to_string(&parsed).ok()?,
        })
    }
}

/// A policy grant's leaf, tagged apart from a gate's and a grant's so none can be
/// read as another.
pub fn leaf(workspace_id: Uuid, grant: &PolicyGrant) -> Hash {
    let mut h = Sha256::new();
    h.update(TAG_POLICY_LEAF);
    h.update(workspace_id.as_bytes());
    // Over the serialized grant rather than field by field: every field decides
    // what it admits, and a struct serializes its fields in one order on both
    // sides.
    h.update(serde_json::to_vec(grant).unwrap_or_default());
    Hash(h.finalize().into())
}

/// One request a policy let out, as the gateway records it.
pub struct Record<'a> {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub grant: &'a PolicyGrant,
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    pub admitted: &'a Admitted,
}

/// Writes the record of each automatic yes, before the request goes out, and
/// says whether every one was written.
///
/// The record is the check. What the token commits to is the policy as it stood
/// when the turn started, so each insert succeeds only if the policy is still
/// `on`, unrevoked, unexpired, at the version committed, set by somebody still in
/// the workspace, and within its caps -- the tighter of the committed caps and
/// the row's own. That is what makes revoking a policy take effect on the next
/// request, whoever is running the turn.
///
/// All or nothing: a request two policies admit through two gates goes out only
/// if both are recorded. The policy row is locked first, so two requests racing
/// for the last of a cap cannot both see room.
pub async fn record(pool: &sqlx::PgPool, records: &[Record<'_>]) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    for r in records {
        sqlx::query(
            "select 1 from approval_policies \
             where workspace_id = $1 and id = $2 for update",
        )
        .bind(r.workspace_id)
        .bind(r.grant.policy_id)
        .execute(&mut *tx)
        .await?;

        let written = sqlx::query(
            "insert into auto_approvals \
                 (workspace_id, id, policy_id, session_id, job_id, requires, \
                  method, host, path, bound, amount, currency, currency_amount) \
             select $1, $3, p.id, $4, $5, $6, $7, $8, $9, $10, $11, $16, $17 \
             from approval_policies p \
             where p.workspace_id = $1 and p.id = $2 \
               and p.effect = 'include' and p.mode = 'on' \
               and p.revoked_at is null \
               and p.expires_at > now() \
               and p.version = $12 \
               and exists ( \
                   select 1 from user_workspace_roles m \
                   where m.user_id = p.created_by and m.workspace_id = p.workspace_id) \
               and (select count(*) from auto_approvals a \
                    where a.workspace_id = $1 and a.policy_id = p.id and a.job_id = $5) \
                   < least(p.per_turn, $13) \
               and (select count(*) from auto_approvals a \
                    where a.workspace_id = $1 and a.policy_id = p.id \
                      and a.at > now() - interval '1 day') \
                   < least(p.per_day, $14) \
               and (p.sum_field is null or \
                    coalesce((select sum(a.amount) from auto_approvals a \
                              where a.workspace_id = $1 and a.policy_id = p.id \
                                and a.at > now() - interval '1 day'), 0) + $11 \
                    <= least(p.sum_per_day, $15)) \
               and ($16::text is null or \
                    coalesce((select sum(a.currency_amount) from auto_approvals a \
                              where a.workspace_id = $1 and a.policy_id = p.id \
                                and a.currency = $16 \
                                and a.at > now() - interval '1 day'), 0) + $17 \
                    <= coalesce(least($18::bigint, (p.daily_totals ->> $16)::bigint), \
                                9223372036854775807))",
        )
        .bind(r.workspace_id)
        .bind(r.grant.policy_id)
        .bind(Uuid::now_v7())
        .bind(r.session_id)
        .bind(r.grant.job_id)
        .bind(&r.grant.requires)
        .bind(r.method)
        .bind(r.host)
        .bind(r.path)
        .bind(&r.admitted.bound)
        .bind(r.admitted.amount)
        .bind(r.grant.version)
        .bind(r.grant.per_turn)
        .bind(r.grant.per_day)
        .bind(r.grant.sum_per_day.unwrap_or(i64::MAX))
        .bind(r.admitted.money.as_ref().map(|(c, _)| c.as_str()))
        .bind(r.admitted.money.as_ref().map(|(_, m)| *m))
        .bind(r.grant.currency_per_day)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if written != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
    }
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(field: &str, op: Op, value: serde_json::Value) -> Test {
        Test {
            field: field.into(),
            op,
            value,
        }
    }

    fn body(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        v.as_object().cloned().unwrap()
    }

    fn workspace() -> Uuid {
        Uuid::from_bytes([3; 16])
    }

    fn charge() -> Gate {
        Gate {
            requires: "charge".into(),
            host: "api.example.com".into(),
            method: "POST".into(),
            path: "/charges".into(),
            identified_by: None,
            binds: vec!["booking_id".into(), "amount_pence".into()],
        }
    }

    fn grant_for(gate: &Gate, tests: Vec<Test>) -> PolicyGrant {
        let mut fields: Vec<String> = gate.binds.clone();
        fields.push("description".into());
        fields.sort();
        PolicyGrant {
            policy_id: Uuid::now_v7(),
            version: 1,
            job_id: Uuid::now_v7(),
            gate: gate.key(workspace()),
            requires: gate.requires.clone(),
            tests,
            fields,
            per_turn: 5,
            per_day: 50,
            sum_field: None,
            sum_per_day: None,
            banding: None,
            max_risk: None,
            currency_per_day: None,
        }
    }

    #[test]
    fn a_comparison_matches_only_a_whole_json_number() {
        let tests = [t("amount_pence", Op::Le, json!(5000))];
        assert!(holds(&tests, &body(json!({"amount_pence": 4999}))));
        for refused in [
            json!({"amount_pence": "4999"}),
            json!({"amount_pence": 4999.0}),
            json!({"amount_pence": 18446744073709551615u64}),
            json!({"amount_pence": null}),
            json!({}),
        ] {
            assert!(!holds(&tests, &body(refused.clone())), "{refused}");
        }
    }

    #[test]
    fn a_bounded_field_is_bounded_below_at_zero() {
        // `≤ 5000` alone admits -500000, a refund dressed as a small charge.
        let tests = [t("amount_pence", Op::Le, json!(5000))];
        assert!(!holds(&tests, &body(json!({"amount_pence": -500000}))));
        assert!(holds(&tests, &body(json!({"amount_pence": 0}))));

        // Unless the policy says otherwise.
        let below = [
            t("amount_pence", Op::Le, json!(5000)),
            t("amount_pence", Op::Ge, json!(-100)),
        ];
        assert!(holds(&below, &body(json!({"amount_pence": -50}))));
        assert!(!holds(&below, &body(json!({"amount_pence": -500}))));
    }

    #[test]
    fn absent_does_not_match_even_an_exclusion() {
        let tests = [
            t("folder", Op::In, json!(["spam", "junk"])),
            t("folder", Op::Ne, json!("junk")),
        ];
        assert!(holds(&tests, &body(json!({"folder": "spam"}))));
        assert!(!holds(&tests, &body(json!({"folder": "junk"}))));
        assert!(!holds(&tests, &body(json!({}))));
        assert!(!holds(&tests, &body(json!({"folder": null}))));
    }

    #[test]
    fn equality_is_by_kind_as_well_as_value() {
        let tests = [t("account", Op::Eq, json!("1"))];
        assert!(!holds(&tests, &body(json!({"account": 1}))));
        assert!(holds(&tests, &body(json!({"account": "1"}))));
    }

    #[test]
    fn an_exclusion_needs_a_positive_test_beside_it() {
        assert!(check_tests(&[t("folder", Op::Ne, json!("inbox"))]).is_err());
        assert!(check_tests(&[t("folder", Op::NotIn, json!(["inbox"]))]).is_err());
        // A positive test on another field is not one beside it.
        assert!(
            check_tests(&[
                t("folder", Op::Ne, json!("inbox")),
                t("account", Op::Eq, json!("a")),
            ])
            .is_err()
        );
        assert!(
            check_tests(&[
                t("folder", Op::Ne, json!("inbox")),
                t("folder", Op::In, json!(["spam", "inbox"])),
            ])
            .is_ok()
        );
    }

    #[test]
    fn a_comparison_bound_is_a_whole_number() {
        assert!(check_tests(&[t("amount_pence", Op::Le, json!("5000"))]).is_err());
        assert!(check_tests(&[t("amount_pence", Op::Le, json!(50.5))]).is_err());
        assert!(check_tests(&[t("amount_pence", Op::Le, json!(5000))]).is_ok());
    }

    #[test]
    fn a_policy_fits_only_a_gate_binding_what_it_tests() {
        let binds = vec!["booking_id".to_string(), "amount_pence".to_string()];
        let numbers = vec!["amount_pence".to_string()];
        let tests = [t("amount_pence", Op::Le, json!(5000))];
        assert!(fits(&tests, None, &binds, &numbers));
        // Compared, but not declared a number.
        assert!(!fits(&tests, None, &binds, &[]));
        // Tested, but not bound by this gate.
        assert!(!fits(
            &[t("currency", Op::Eq, json!("gbp"))],
            None,
            &binds,
            &numbers
        ));
        assert!(!fits(&[], Some("booking_id"), &binds, &numbers));
    }

    #[test]
    fn a_policy_admits_a_request_its_tests_hold_of() {
        let gate = charge();
        let grant = grant_for(&gate, vec![t("amount_pence", Op::Le, json!(5000))]);
        let admitted = grant
            .admits(
                workspace(),
                &gate,
                None,
                Some(r#"{"booking_id":"bk_1","amount_pence":4000,"description":"x"}"#),
            )
            .expect("admitted");
        assert_eq!(
            admitted.bound,
            json!({"booking_id": "bk_1", "amount_pence": 4000})
        );
        assert!(
            grant
                .admits(
                    workspace(),
                    &gate,
                    None,
                    Some(r#"{"booking_id":"bk_1","amount_pence":400000}"#)
                )
                .is_none()
        );
    }

    #[test]
    fn a_field_nobody_called_harmless_asks_a_person() {
        let gate = charge();
        let grant = grant_for(&gate, vec![]);
        assert!(
            grant
                .admits(
                    workspace(),
                    &gate,
                    None,
                    Some(r#"{"booking_id":"bk_1","amount_pence":1,"bcc":"x@example.com"}"#)
                )
                .is_none()
        );
    }

    #[test]
    fn a_query_string_or_a_case_variant_key_asks_a_person() {
        let gate = charge();
        let mut grant = grant_for(&gate, vec![]);
        let ok = r#"{"booking_id":"bk_1","amount_pence":1}"#;
        assert!(grant.admits(workspace(), &gate, None, Some(ok)).is_some());
        assert!(
            grant
                .admits(workspace(), &gate, Some("amount_pence=9"), Some(ok))
                .is_none()
        );
        // Even where the variant were listed as free.
        grant.fields.push("Amount_Pence".into());
        assert!(
            grant
                .admits(
                    workspace(),
                    &gate,
                    None,
                    Some(r#"{"booking_id":"bk_1","amount_pence":1,"Amount_Pence":900}"#)
                )
                .is_none()
        );
    }

    #[test]
    fn what_goes_out_is_what_was_tested() {
        // Named twice: the parse keeps one, and the body sent is that parse, so
        // a remote API reading the other copy never sees it.
        let gate = charge();
        let grant = grant_for(&gate, vec![t("amount_pence", Op::Le, json!(5000))]);
        let admitted = grant
            .admits(
                workspace(),
                &gate,
                None,
                Some(r#"{"booking_id":"bk_1","amount_pence":900000,"amount_pence":40}"#),
            )
            .expect("the parse admits");
        let sent: serde_json::Value = serde_json::from_str(&admitted.body).unwrap();
        assert_eq!(sent["amount_pence"], json!(40));
        assert_eq!(admitted.body.matches("amount_pence").count(), 1);
    }

    #[test]
    fn a_policy_says_nothing_about_another_gate() {
        // Same `requires`, another skill's operation: committed against the
        // gate's own leaf, so it does not carry over.
        let gate = charge();
        let grant = grant_for(&gate, vec![]);
        let mut other = charge();
        other.path = "/payments".into();
        assert!(
            grant
                .admits(
                    workspace(),
                    &other,
                    None,
                    Some(r#"{"booking_id":"bk_1","amount_pence":1}"#)
                )
                .is_none()
        );
    }

    #[test]
    fn a_policy_says_nothing_in_another_workspace() {
        let gate = charge();
        let grant = grant_for(&gate, vec![]);
        assert!(
            grant
                .admits(
                    Uuid::now_v7(),
                    &gate,
                    None,
                    Some(r#"{"booking_id":"bk_1","amount_pence":1}"#)
                )
                .is_none()
        );
    }

    #[test]
    fn a_host_gate_is_never_covered() {
        let reach = Gate {
            requires: super::super::gate::REACH.into(),
            host: "api.example.com".into(),
            method: "GET".into(),
            path: "/*".into(),
            identified_by: None,
            binds: vec![],
        };
        let grant = grant_for(&reach, vec![]);
        assert!(
            grant
                .admits(workspace(), &reach, None, Some("{}"))
                .is_none()
        );
    }

    #[test]
    fn a_summed_field_must_be_a_whole_number_not_below_zero() {
        let gate = charge();
        let mut grant = grant_for(&gate, vec![t("amount_pence", Op::Le, json!(5000))]);
        grant.sum_field = Some("amount_pence".into());
        grant.sum_per_day = Some(10_000);
        let admitted = grant
            .admits(
                workspace(),
                &gate,
                None,
                Some(r#"{"booking_id":"bk_1","amount_pence":40}"#),
            )
            .unwrap();
        assert_eq!(admitted.amount, Some(40));
    }

    fn banded(grant: &mut PolicyGrant, max: Risk) {
        grant.banding = Some(super::super::amount::Banding {
            field: "amount_pence".into(),
            currency: "GBP".into(),
            currency_field: None,
            minor_units: 2,
            base: Risk::High,
            bands: super::super::amount::Bands {
                low: Some(5000),
                medium: Some(20000),
            },
            floor: Default::default(),
        });
        grant.max_risk = Some(max);
    }

    #[test]
    fn a_banded_policy_admits_only_up_to_its_risk() {
        let gate = charge();
        let mut grant = grant_for(&gate, vec![]);
        banded(&mut grant, Risk::Low);
        let charge = |pence: serde_json::Value| {
            grant.admits(
                workspace(),
                &gate,
                None,
                Some(&json!({"booking_id": "bk_1", "amount_pence": pence}).to_string()),
            )
        };
        let admitted = charge(json!(4999)).expect("low");
        assert_eq!(admitted.money, Some(("GBP".to_string(), 4999)));
        assert!(charge(json!(5000)).is_none(), "medium");
        // An amount nobody can trust is the base risk, high.
        assert!(charge(json!("10")).is_none());
        assert!(charge(json!(-10)).is_none());
    }

    #[test]
    fn a_day_is_counted_in_the_currencys_standard_units() {
        let gate = charge();
        let mut grant = grant_for(&gate, vec![]);
        banded(&mut grant, Risk::High);
        // An operation counting in whole pounds: £49 is 4,900 pence of the day.
        let banding = grant.banding.as_mut().unwrap();
        banding.minor_units = 0;
        banding.bands = super::super::amount::Bands {
            low: Some(50),
            medium: Some(200),
        };
        let admitted = grant
            .admits(
                workspace(),
                &gate,
                None,
                Some(r#"{"booking_id":"bk_1","amount_pence":49}"#),
            )
            .unwrap();
        assert_eq!(admitted.money, Some(("GBP".to_string(), 4900)));
    }

    #[test]
    fn a_daily_total_needs_an_amount_it_can_count() {
        let gate = charge();
        let mut grant = grant_for(&gate, vec![]);
        banded(&mut grant, Risk::High);
        grant.currency_per_day = Some(50000);
        let ok = r#"{"booking_id":"bk_1","amount_pence":10}"#;
        assert!(grant.admits(workspace(), &gate, None, Some(ok)).is_some());
        // High, which the policy reaches, but not countable against the total.
        assert!(
            grant
                .admits(
                    workspace(),
                    &gate,
                    None,
                    Some(r#"{"booking_id":"bk_1","amount_pence":"10"}"#)
                )
                .is_none()
        );
    }

    #[test]
    fn a_grant_without_bands_commits_what_it_did_before() {
        // The new fields are left out when absent, so a grant for a gate with no
        // amount hashes as it did, and a runtime that predates them carries it
        // unchanged.
        let gate = charge();
        let grant = grant_for(&gate, vec![]);
        let text = serde_json::to_string(&grant).unwrap();
        assert!(
            !text.contains("banding") && !text.contains("max_risk"),
            "{text}"
        );
    }
}
