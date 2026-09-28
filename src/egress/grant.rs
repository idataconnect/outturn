//! What a yes permits, as the tier making the request checks it.
//!
//! See `docs/approvals.md`. A hold answers "may this turn proceed"; a grant
//! answers "may this request go out". Two questions, and releasing the hold is
//! not answering the second -- without a grant a resumed turn reaches the same
//! gate and is refused a second time.
//!
//! Beside `gate.rs` rather than under `api::` because both tiers read this. The
//! API writes grants and commits to them; the gateway receives them in the turn
//! token and checks them against the request it is about to make. The store that
//! persists them stays in `api::grant`, which is the half only one tier needs.
//!
//! **A grant is keyed on what made the request the one somebody looked at.** An
//! approver sees a rendered request -- charge £120 to Visa 4471 for booking
//! bk_8812 -- and says yes to that. So the digest covers the act, the method, the
//! host, the path, and the value of every field the skill declared in `binds`.
//! Keying on method, host and path alone was the bug this replaces: a retry is
//! the same shape by construction, but so is a charge for a different amount, and
//! a £40 approval let £4,000 through.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::gate::Gate;

/// How far a grant reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Extent {
    /// The request somebody approved, and anything indistinguishable from it.
    /// The default.
    Call,
    /// Every declared act of one `requires` against one identified unit, for the
    /// rest of this turn. What a ticked `covers` grants, and never inferred from
    /// what was asked.
    Unit,
}

impl Extent {
    pub fn as_str(self) -> &'static str {
        match self {
            Extent::Call => "call",
            Extent::Unit => "unit",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "call" => Some(Extent::Call),
            "unit" => Some(Extent::Unit),
            _ => None,
        }
    }
}

/// What identifies an outbound request, for hashing and for matching.
///
/// Four values, and they are the same four at every tier: the gateway checks
/// the live request against them, `api::gated` records them when it raises an
/// approval, and the projection recovers them from a stored call to ask whether
/// a grant covers it. The *hashing* was always shared; what was not was the
/// mapping from a call to these four, and that is what this type names.
pub struct Shape {
    pub method: String,
    pub host: String,
    pub path: String,
    pub body: Option<String>,
}

impl Shape {
    /// The shape of a `fetch_url` call, as the guest recorded its arguments.
    ///
    /// One interpretation, so nothing re-derives it. The guest that builds the
    /// request from a model's arguments lives in another crate -- `agents/default`,
    /// compiled to wasm with its artifact committed -- so this cannot literally
    /// be the code it runs. What it can be is the only *host-side* reading of
    /// those arguments, matched to the guest's by the test below.
    ///
    /// `None` wherever the answer is not plain, and every caller treats that as
    /// a call it cannot check rather than one it may permit. In particular the
    /// method is not defaulted: `fetch_url` defaults an absent method to `GET`
    /// before sending, but a stored call with no method is one whose real method
    /// nobody knows, and guessing it could land inside a `GET` gate.
    pub fn of_fetch_arguments(arguments: &serde_json::Value) -> Option<Self> {
        let arguments = arguments.as_str()?;
        let call: serde_json::Value = serde_json::from_str(arguments).ok()?;

        // `reqwest::Url`, because that is what the gateway parses with and
        // `normalise_path` is documented against its resolution of `.` and
        // `..`. A different parser could agree on the string and disagree on
        // the path that gets hashed.
        let url = reqwest::Url::parse(call["url"].as_str()?).ok()?;

        Some(Self {
            method: call["method"].as_str()?.to_ascii_uppercase(),
            // Without the port, which is what every other tier hashes: they all
            // read `host_str()`.
            host: url.host_str()?.to_string(),
            path: url.path().to_string(),
            body: call["body"].as_str().map(str::to_string),
        })
    }
}

/// The key a `call` grant is taken out on, over the request a person was shown.
///
/// Every bound field is hashed in the order the skill declared them, so two
/// skills declaring the same fields in different orders cannot collide, and
/// reordering a declaration invalidates grants taken out under the old one --
/// which is the safe direction.
///
/// A field the request does not carry is hashed as absent, distinctly from one
/// carrying the empty string: otherwise omitting a field would be a way to
/// collide with a grant taken out when it was present.
pub fn digest(gate: &Gate, method: &str, host: &str, path: &str, body: Option<&str>) -> String {
    let parsed: Option<serde_json::Value> = body.and_then(|b| serde_json::from_str(b).ok());

    let mut hasher = Sha256::new();
    hasher.update(b"outturn:grant:digest:v1\0");
    hasher.update(gate.requires.as_bytes());
    hasher.update([0]);
    hasher.update(method.to_ascii_uppercase().as_bytes());
    hasher.update([0]);
    hasher.update(host.to_ascii_lowercase().as_bytes());
    hasher.update([0]);
    hasher.update(super::gate::normalise_path(path).as_bytes());

    for field in &gate.binds {
        hasher.update([0]);
        hasher.update(field.as_bytes());
        hasher.update([0]);
        match parsed.as_ref().and_then(|v| v.get(field)) {
            // Tagged by kind as well as by value, so the string "1" and the
            // number 1 are different approvals. A body that can choose its own
            // spelling of a value could otherwise reach a grant taken out on the
            // other one.
            Some(serde_json::Value::String(s)) => {
                hasher.update(b"s");
                hasher.update(s.as_bytes());
            }
            Some(serde_json::Value::Number(n)) => {
                hasher.update(b"n");
                hasher.update(n.to_string().as_bytes());
            }
            Some(serde_json::Value::Bool(b)) => {
                hasher.update(b"b");
                hasher.update(if *b { b"1" } else { b"0" });
            }
            Some(serde_json::Value::Null) | None => hasher.update(b"-"),
            // An object or an array where a scalar was declared. Hashed as its
            // own kind rather than refused here: `binds` is validated at publish,
            // and a request that arrives with the wrong shape must not be able to
            // match a grant taken out on a scalar.
            Some(_) => hasher.update(b"?"),
        }
    }
    hex::encode(hasher.finalize())
}

/// The unit a `covers` grant is keyed on, read out of the request body.
///
/// Only a top-level field, and only a scalar, for the reason `binds` gives: the
/// value a security decision is keyed on is not the place to grow a query
/// language.
pub fn unit_from_body(body: Option<&str>, field: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body?).ok()?;
    match parsed.get(field)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Whether a request carries every field the gate says its approval is keyed
/// on, naming the first one it does not.
///
/// Refused rather than digested as absent, which is what this used to do. A
/// declaration saying `amount_pence` is what makes a charge the charge somebody
/// approved means a charge without one is malformed, not a different charge --
/// and treating it as different produced a *valid* digest that matched no grant,
/// so the request was refused a second time and a second approval was raised for
/// a request nobody could sensibly approve. Watched happen: a resumed turn sent
/// a charge with no `payment_account_id`, and the person was asked again with no
/// way to tell why.
///
/// It also removes a reachable key. "Absent" hashing to something stable means a
/// grant can be taken out on a request missing a field and later cover another
/// one missing the same field, which is a class of match nobody declared.
///
/// An empty declaration is legal and always satisfied: the `reach` gate binds
/// nothing, because what a person approves there is a host rather than anything
/// in a body.
pub fn missing_bound_field(gate: &Gate, body: Option<&str>) -> Option<String> {
    // No guard for the empty declaration: a search over nothing finds nothing,
    // so the `reach` gate is satisfied by construction. An early return here
    // would read as the thing keeping it legal and would not be -- removing it
    // changes no behaviour, which is how it was found.
    let parsed: Option<serde_json::Value> = body.and_then(|b| serde_json::from_str(b).ok());
    gate.binds
        .iter()
        .find(|field| {
            match parsed.as_ref().and_then(|v| v.get(field.as_str())) {
                Some(serde_json::Value::String(_))
                | Some(serde_json::Value::Number(_))
                | Some(serde_json::Value::Bool(_)) => false,
                // An explicit null is not a value, so it is absent by another
                // spelling. An object or an array where a scalar was declared is
                // the `binds` rule -- top-level scalars only -- being enforced
                // where somebody can see it rather than silently digested as a
                // shape of its own.
                _ => true,
            }
        })
        .cloned()
}

/// A grant as it travels, and as the gateway checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Granted {
    pub requires: String,
    pub extent: Extent,
    /// The `binds` digest for a `call` grant; the unit's value for a `unit` one.
    pub keyed_on: String,
}

impl Granted {
    /// Whether this grant permits the request in hand.
    ///
    /// The gate is passed in because both extents are keyed on something the
    /// *declaration* named -- the bound fields, or `identified_by` -- and never
    /// on anything the caller chose to call it.
    pub fn permits(
        &self,
        gate: &Gate,
        method: &str,
        host: &str,
        path: &str,
        body: Option<&str>,
    ) -> bool {
        if self.requires != gate.requires {
            return false;
        }
        match self.extent {
            Extent::Call => self.keyed_on == digest(gate, method, host, path, body),
            Extent::Unit => gate
                .identified_by
                .as_deref()
                .and_then(|field| unit_from_body(body, field))
                .is_some_and(|unit| unit == self.keyed_on),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charge_gate() -> Gate {
        Gate {
            requires: "charge".into(),
            host: "api.guesthouse.test".into(),
            method: "POST".into(),
            path: "/charges".into(),
            identified_by: Some("booking_id".into()),
            binds: vec![
                "payment_account_id".into(),
                "booking_id".into(),
                "amount_pence".into(),
            ],
        }
    }

    fn body(account: &str, booking: &str, amount: i64) -> String {
        format!(
            r#"{{"payment_account_id":"{account}","booking_id":"{booking}","amount_pence":{amount}}}"#
        )
    }

    fn call_grant(gate: &Gate, body: &str) -> Granted {
        Granted {
            requires: gate.requires.clone(),
            extent: Extent::Call,
            keyed_on: digest(gate, "POST", "api.guesthouse.test", "/charges", Some(body)),
        }
    }

    /// The bug this keying exists to stop. Keyed on method, host and path alone
    /// -- which reads as sufficient, because a retry is the same shape -- a yes
    /// to £40 let £4,000 through, because nothing in the key mentioned the
    /// amount.
    #[test]
    fn a_grant_for_one_charge_does_not_cover_a_larger_one() {
        let gate = charge_gate();
        let approved = body("pa_4471", "bk_8812", 4_000);
        let grant = call_grant(&gate, &approved);

        let larger = body("pa_4471", "bk_8812", 400_000);
        assert!(
            !grant.permits(
                &gate,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(&larger)
            ),
            "a grant must not cover a charge for a different amount"
        );
    }

    /// Nor one to somebody else, or against another booking.
    #[test]
    fn a_grant_is_bound_to_every_field_it_declares() {
        let gate = charge_gate();
        let approved = body("pa_4471", "bk_8812", 12_000);
        let grant = call_grant(&gate, &approved);

        for other in [
            body("pa_8802", "bk_8812", 12_000),
            body("pa_4471", "bk_9000", 12_000),
        ] {
            assert!(
                !grant.permits(
                    &gate,
                    "POST",
                    "api.guesthouse.test",
                    "/charges",
                    Some(&other)
                ),
                "a bound field that differs must not be covered: {other}"
            );
        }
    }

    /// And the retry it exists for goes through.
    #[test]
    fn a_grant_covers_the_retry_of_the_call_it_was_given_for() {
        let gate = charge_gate();
        let approved = body("pa_4471", "bk_8812", 12_000);
        let grant = call_grant(&gate, &approved);

        assert!(
            grant.permits(
                &gate,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(&approved)
            ),
            "the approved request itself must be covered"
        );
        // The spellings a gate already treats as one path, and a method the
        // request normalised differently.
        assert!(
            grant.permits(
                &gate,
                "post",
                "API.guesthouse.test",
                "//charges/",
                Some(&approved)
            ),
            "a retry must not be refused over case or a trailing slash"
        );
    }

    /// A grant for one act never covers another, however close they look.
    #[test]
    fn a_grant_does_not_cross_acts() {
        let gate = charge_gate();
        let approved = body("pa_4471", "bk_8812", 12_000);
        let grant = call_grant(&gate, &approved);

        let refund = Gate {
            requires: "refund".into(),
            ..charge_gate()
        };
        assert!(
            !grant.permits(
                &refund,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(&approved)
            ),
            "approving a charge must not approve a refund"
        );
    }

    /// A missing field is not the empty string. Otherwise dropping a field would
    /// be a way to collide with a grant taken out when it was present.
    #[test]
    fn an_absent_field_is_not_an_empty_one() {
        let gate = charge_gate();
        let absent = r#"{"payment_account_id":"pa_4471","booking_id":"bk_8812"}"#;
        let empty = r#"{"payment_account_id":"pa_4471","booking_id":"bk_8812","amount_pence":""}"#;
        assert_ne!(
            digest(
                &gate,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(absent)
            ),
            digest(
                &gate,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(empty)
            ),
        );
    }

    /// Nor is the number 1 the string "1": a body that could choose its own
    /// spelling could otherwise reach a grant taken out on the other.
    #[test]
    fn a_value_is_bound_to_its_kind() {
        let gate = charge_gate();
        let number = body("pa_4471", "bk_8812", 12_000);
        let string =
            r#"{"payment_account_id":"pa_4471","booking_id":"bk_8812","amount_pence":"12000"}"#;
        assert_ne!(
            digest(
                &gate,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(&number)
            ),
            digest(
                &gate,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(string)
            ),
        );
    }

    /// Reordering a declaration invalidates the grants taken out under it, which
    /// is the safe direction: a grant whose meaning quietly changed is worse
    /// than one that has to be asked for again.
    #[test]
    fn reordering_the_declaration_changes_the_key() {
        let gate = charge_gate();
        let mut reordered = charge_gate();
        reordered.binds.reverse();
        let payload = body("pa_4471", "bk_8812", 12_000);
        assert_ne!(
            digest(
                &gate,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(&payload)
            ),
            digest(
                &reordered,
                "POST",
                "api.guesthouse.test",
                "/charges",
                Some(&payload)
            ),
        );
    }

    /// A unit grant spans the act against one unit, whatever else varies --
    /// which is what makes it worth ticking, and why it is never inferred.
    #[test]
    fn a_unit_grant_spans_its_unit_and_stops_there() {
        let gate = charge_gate();
        let grant = Granted {
            requires: "charge".into(),
            extent: Extent::Unit,
            keyed_on: "bk_8812".into(),
        };

        // A different amount for the approved booking is covered.
        let more = body("pa_4471", "bk_8812", 999_999);
        assert!(grant.permits(
            &gate,
            "POST",
            "api.guesthouse.test",
            "/charges",
            Some(&more)
        ));

        // The next booking along is not.
        let other = body("pa_4471", "bk_9000", 12_000);
        assert!(!grant.permits(
            &gate,
            "POST",
            "api.guesthouse.test",
            "/charges",
            Some(&other)
        ));
    }

    /// A body that is not JSON, or a field that is not a scalar, must not be
    /// able to match a grant taken out on one.
    #[test]
    fn an_unreadable_body_matches_nothing_taken_out_on_a_real_one() {
        let gate = charge_gate();
        let approved = body("pa_4471", "bk_8812", 12_000);
        let grant = call_grant(&gate, &approved);

        for bad in [
            "not json at all",
            r#"{"payment_account_id":{"nested":1},"booking_id":"bk_8812","amount_pence":12000}"#,
        ] {
            assert!(
                !grant.permits(&gate, "POST", "api.guesthouse.test", "/charges", Some(bad)),
                "{bad} must not be covered"
            );
        }
    }

    /// The failure this check exists for, watched in a live turn.
    ///
    /// A resumed turn sent its charge without `payment_account_id`. Digested as
    /// absent, that was a *valid* key matching no grant, so the gate refused it
    /// and raised a second approval -- for a request nobody could sensibly
    /// approve, with nothing saying why.
    #[test]
    fn a_request_missing_a_bound_field_is_named_rather_than_keyed() {
        let gate = charge_gate();
        let without = r#"{"booking_id":"bk_8812","amount_pence":9000}"#;
        assert_eq!(
            missing_bound_field(&gate, Some(without)).as_deref(),
            Some("payment_account_id"),
        );
    }

    /// An explicit null is absence by another spelling.
    #[test]
    fn a_null_is_not_a_value() {
        let gate = charge_gate();
        let nulled = r#"{"payment_account_id":null,"booking_id":"bk_8812","amount_pence":9000}"#;
        assert_eq!(
            missing_bound_field(&gate, Some(nulled)).as_deref(),
            Some("payment_account_id"),
        );
    }

    /// And an object where a scalar was declared, which is the `binds` rule
    /// enforced where somebody can see it.
    #[test]
    fn a_non_scalar_where_a_scalar_was_declared_is_refused() {
        let gate = charge_gate();
        let nested =
            r#"{"payment_account_id":{"id":"pa_4471"},"booking_id":"bk_8812","amount_pence":9000}"#;
        assert_eq!(
            missing_bound_field(&gate, Some(nested)).as_deref(),
            Some("payment_account_id"),
        );
    }

    /// A body that is not JSON at all fails on the first declared field rather
    /// than being let through as "nothing to check".
    #[test]
    fn an_unparseable_body_is_missing_everything() {
        let gate = charge_gate();
        assert!(missing_bound_field(&gate, Some("not json")).is_some());
        assert!(missing_bound_field(&gate, None).is_some());
    }

    /// A complete request passes, or the check would refuse the very thing it
    /// exists to let through.
    #[test]
    fn a_complete_request_is_not_missing_anything() {
        let gate = charge_gate();
        let whole = body("pa_4471", "bk_8812", 9000);
        assert!(missing_bound_field(&gate, Some(&whole)).is_none());
    }

    /// A gate that binds nothing is always satisfied. `for_unreviewed_hosts`
    /// emits exactly that: what a person approves there is a host, and a `GET`
    /// carries no body to bind.
    #[test]
    fn a_gate_that_binds_nothing_is_always_satisfied() {
        let reach = Gate {
            requires: "reach".into(),
            host: "api.example.com".into(),
            method: "GET".into(),
            path: "/*".into(),
            identified_by: None,
            binds: Vec::new(),
        };
        assert!(missing_bound_field(&reach, None).is_none());
        assert!(missing_bound_field(&reach, Some("{}")).is_none());
    }
}

#[cfg(test)]
mod shape_matches_the_guest {
    use super::Shape;

    /// What the guest actually stores, copied from a real refused charge.
    const REAL: &str = r#"{"body":"{\"payment_account_id\":\"pa_4471\",\"booking_id\":\"bk_01a0e6f308ef\",\"amount_pence\":19500,\"idempotency_key\":\"charge-bk_01a0e6f308ef-19500\"}","headers":{"Content-Type":"application/json"},"method":"POST","url":"http://outturn-hollowbrook:8084/charges"}"#;

    /// The four values every tier hashes, read off a real stored call.
    ///
    /// The guest that built this request lives in another crate and cannot
    /// share this code, so this test is what holds the two readings together.
    /// If `fetch_url` changes how it interprets a model's arguments -- accepting
    /// an object body, an aliased url key, a defaulted method -- this is what
    /// should fail, because the alternative is that nothing fails and approved
    /// calls silently stop being retracted.
    #[test]
    fn the_four_values_come_back_as_the_guest_sent_them() {
        let shape = Shape::of_fetch_arguments(&serde_json::json!(REAL)).expect("shape");

        assert_eq!(shape.method, "POST");
        // Without the port: every tier hashes `host_str()`.
        assert_eq!(shape.host, "outturn-hollowbrook");
        assert_eq!(shape.path, "/charges");
        assert!(
            shape
                .body
                .as_deref()
                .is_some_and(|b| b.contains("\"amount_pence\":19500")),
            "the body goes through byte for byte, since the digest hashes its fields"
        );
    }

    /// A method is never guessed. `fetch_url` defaults an absent one to `GET`
    /// before sending, but a *stored* call with no method is one whose real
    /// method nobody knows -- and guessing could land it inside a `GET` gate.
    #[test]
    fn a_missing_method_is_not_defaulted() {
        let no_method = serde_json::json!(r#"{"url":"http://somewhere/x","body":"{}"}"#);
        assert!(Shape::of_fetch_arguments(&no_method).is_none());
    }

    #[test]
    fn anything_unreadable_is_no_shape_at_all() {
        for bad in [
            serde_json::json!({"method": "POST"}),
            serde_json::json!("{not json"),
            serde_json::json!(r#"{"method":"POST"}"#),
            serde_json::json!(r#"{"method":"POST","url":"not a url"}"#),
            serde_json::json!(r#"{"method":"POST","url":"file:///etc/passwd"}"#),
        ] {
            assert!(
                Shape::of_fetch_arguments(&bad).is_none(),
                "unreadable arguments must yield no shape: {bad}"
            );
        }
    }

    /// A body the guest could not read is sent as no body, and hashed as none.
    #[test]
    fn a_body_that_is_not_a_string_is_no_body() {
        let object_body =
            serde_json::json!(r#"{"method":"POST","url":"http://somewhere/x","body":{"a":1}}"#);
        let shape = Shape::of_fetch_arguments(&object_body).expect("shape");
        assert!(shape.body.is_none());
    }
}
