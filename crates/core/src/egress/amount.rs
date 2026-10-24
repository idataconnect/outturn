//! Risk worked out from what a request asks for, rather than fixed on its
//! operation.
//!
//! See `docs/auto-approval.md`, "Risk from the amount". An operation that moves
//! money declares which bound field is the amount and in what currency, with
//! default bands -- upper bounds, one per level below its base risk -- and an
//! optional floor from which a level holds whatever a workspace says. A
//! workspace moves the bands either way, never past the floor. What a request's
//! risk is follows from those and the amount it carries, and is worked out at
//! the gateway from bands committed in the turn token, so the agent cannot argue
//! its way into a lower band.
//!
//! Beside `policy.rs` because both tiers read it: the API validates what is
//! declared and set and commits the bands a turn runs under, and the gateway
//! applies them to the request in hand.

use serde::{Deserialize, Serialize};

use super::policy::Risk;

/// The most digits after the point a currency is written with. Four covers
/// every currency in use and leaves nothing that reads as a typo.
pub const MAX_MINOR_UNITS: u8 = 4;

/// Which bound field is the amount, and in what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    pub field: String,
    /// Three upper-case letters, `GBP`.
    pub currency: String,
    /// How many of the field's units make a digit after the point: 2 for pence.
    pub minor_units: u8,
    /// The bound field a request names its currency in, where it names one.
    /// A request whose value there is not exactly `currency` carries an amount
    /// nobody can trust: "5000" of pence and "5000" of yen are not one risk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency_field: Option<String>,
}

/// Upper bounds, one per level, in the amount field's own units. Below `low`,
/// low; below `medium`, medium; above every band, the operation's base risk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bands {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<i64>,
}

/// From each amount, at least that level, whatever the bands say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<i64>,
}

/// What an operation declares about money, stored beside its gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub amount: Amount,
    #[serde(default)]
    pub bands: Bands,
    #[serde(default)]
    pub floor: Floor,
}

impl Bands {
    pub fn get(&self, level: Risk) -> Option<i64> {
        match level {
            Risk::Low => self.low,
            Risk::Medium => self.medium,
            Risk::High => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.low.is_none() && self.medium.is_none()
    }

    /// Whether these bands can stand on an operation of this base risk under
    /// this floor: each one above zero, below the base risk, rising with the
    /// level, and none reaching past the floor of a level above it.
    ///
    /// The same check for the operator's defaults and a workspace's own, so a
    /// workspace trying to call £10,000 low where the floor says high is told so
    /// rather than quietly held.
    pub fn check(&self, base: Risk, floor: &Floor) -> Result<(), String> {
        for (level, band) in [(Risk::Low, self.low), (Risk::Medium, self.medium)] {
            let Some(band) = band else { continue };
            if band <= 0 {
                return Err(format!(
                    "the {} band is an amount above zero",
                    level.as_str()
                ));
            }
            if level >= base {
                return Err(format!(
                    "a {} band means nothing on an operation whose risk is {}",
                    level.as_str(),
                    base.as_str()
                ));
            }
        }
        if let (Some(low), Some(medium)) = (self.low, self.medium)
            && low >= medium
        {
            return Err("the bands rise with the level: low below medium".into());
        }
        for (level, at) in [(Risk::Medium, floor.medium), (Risk::High, floor.high)] {
            let Some(at) = at else { continue };
            for below in [Risk::Low, Risk::Medium].into_iter().filter(|b| *b < level) {
                if self.get(below).is_some_and(|band| band > at) {
                    return Err(format!(
                        "the {} band reaches past the floor, from which every amount is {}",
                        below.as_str(),
                        level.as_str()
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Floor {
    pub fn get(&self, level: Risk) -> Option<i64> {
        match level {
            Risk::Low => None,
            Risk::Medium => self.medium,
            Risk::High => self.high,
        }
    }

    pub fn check(&self) -> Result<(), String> {
        for (level, at) in [(Risk::Medium, self.medium), (Risk::High, self.high)] {
            if at.is_some_and(|a| a <= 0) {
                return Err(format!(
                    "the {} floor is an amount above zero",
                    level.as_str()
                ));
            }
        }
        if let (Some(medium), Some(high)) = (self.medium, self.high)
            && medium >= high
        {
            return Err("the floors rise with the level: medium below high".into());
        }
        Ok(())
    }
}

/// Three upper-case letters. Never a symbol, which several currencies share.
pub fn check_currency(currency: &str) -> Result<(), String> {
    if currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(format!(
            "a currency is three upper-case letters, GBP or USD, not {currency}"
        ))
    }
}

impl Declaration {
    /// Whether this is a declaration that can mean something on a gate binding
    /// these fields, at this base risk.
    pub fn check(&self, base: Risk, binds: &[String], numbers: &[String]) -> Result<(), String> {
        let field = &self.amount.field;
        if !binds.contains(field) {
            return Err(format!(
                "the amount is {field}, which the approval does not bind"
            ));
        }
        // Read as the comparisons are, so it has to be one of the fields they may
        // compare.
        if !numbers.contains(field) {
            return Err(format!(
                "the amount is {field}, which numbers does not list"
            ));
        }
        if let Some(named) = &self.amount.currency_field {
            if !binds.contains(named) {
                return Err(format!(
                    "the currency is named in {named}, which the approval does not bind"
                ));
            }
            if named == field {
                return Err("the currency and the amount are two fields".into());
            }
        }
        check_currency(&self.amount.currency)?;
        if self.amount.minor_units > MAX_MINOR_UNITS {
            return Err(format!(
                "minor_units is at most {MAX_MINOR_UNITS}: the digits after the point"
            ));
        }
        self.floor.check()?;
        self.bands.check(base, &self.floor)
    }
}

/// The amount a body carries, read as the comparisons read a number: a JSON
/// number, whole and in range, at least zero. Anything else is no amount.
pub fn read(body: &serde_json::Map<String, serde_json::Value>, field: &str) -> Option<i64> {
    match body.get(field)? {
        serde_json::Value::Number(n) => n.as_i64().filter(|v| *v >= 0),
        _ => None,
    }
}

/// What a request's risk is, from the operation's base risk, the bands it runs
/// under, its floor, and the amount the request carries.
///
/// No amount anybody can trust is the base risk, the conservative reading.
/// Otherwise the lowest level below the base whose band the amount is below;
/// above every band, the base; and at or above a floor, at least the floor's
/// level, whatever the bands say.
pub fn risk(base: Risk, bands: &Bands, floor: &Floor, amount: Option<i64>) -> Risk {
    let Some(amount) = amount else {
        return base;
    };
    let mut risk = [Risk::Low, Risk::Medium]
        .into_iter()
        .filter(|level| *level < base)
        .find(|level| bands.get(*level).is_some_and(|band| amount < band))
        .unwrap_or(base);
    for level in [Risk::Medium, Risk::High] {
        if floor.get(level).is_some_and(|at| amount >= at) {
            risk = risk.max(level);
        }
    }
    risk
}

/// The bands one operation runs under in one turn, as the token commits them.
///
/// The workspace's, where it set its own, otherwise the operator's defaults --
/// resolved by the API when it prepares the turn, so the gateway reads nothing
/// but what is signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Banding {
    pub field: String,
    pub currency: String,
    /// Where the request names its currency, if the operation says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency_field: Option<String>,
    pub base: Risk,
    pub bands: Bands,
    pub floor: Floor,
}

impl Banding {
    /// The request's risk, and the amount it was worked out from where there
    /// was one to trust.
    ///
    /// Where the operation names a currency field, an amount is trusted only
    /// beside exactly the declared currency there: any other, a lower-case one,
    /// or none at all is an amount in something the bands say nothing about.
    pub fn assess(&self, body: &serde_json::Map<String, serde_json::Value>) -> (Risk, Option<i64>) {
        let in_currency = self.currency_field.as_ref().is_none_or(|named| {
            matches!(body.get(named), Some(serde_json::Value::String(c)) if *c == self.currency)
        });
        let amount = read(body, &self.field).filter(|_| in_currency);
        (risk(self.base, &self.bands, &self.floor, amount), amount)
    }

    /// The lowest risk any request through this operation can have, which is
    /// what decides whether a selector reaches it at all before a request
    /// arrives.
    pub fn lowest(&self) -> Risk {
        risk(self.base, &self.bands, &self.floor, Some(0))
    }
}

/// An amount in a currency's own units, as a person reads it: `£50.00`,
/// `$1,250.00`, `CHF 12.50`.
pub fn format(amount: i64, currency: &str, minor_units: u8) -> String {
    let scale = 10i64.pow(u32::from(minor_units.min(MAX_MINOR_UNITS)));
    let sign = if amount < 0 { "-" } else { "" };
    let whole = (amount / scale).unsigned_abs();
    let part = (amount % scale).unsigned_abs();
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let number = if minor_units == 0 {
        grouped
    } else {
        format!("{grouped}.{part:0width$}", width = usize::from(minor_units))
    };
    match symbol(currency) {
        Some(symbol) => format!("{sign}{symbol}{number}"),
        None => format!("{sign}{currency} {number}"),
    }
}

/// The symbols worth writing in place of a code: one each, and unambiguous
/// among the currencies listed. Anything else is written as its code.
fn symbol(currency: &str) -> Option<&'static str> {
    Some(match currency {
        "GBP" => "£",
        "USD" => "$",
        "EUR" => "€",
        "JPY" => "¥",
        "INR" => "₹",
        "AUD" => "A$",
        "CAD" => "CA$",
        "NZD" => "NZ$",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn charge() -> Declaration {
        Declaration {
            amount: Amount {
                field: "amount_pence".into(),
                currency: "GBP".into(),
                minor_units: 2,
                currency_field: None,
            },
            bands: Bands {
                low: Some(5000),
                medium: Some(20000),
            },
            floor: Floor {
                medium: None,
                high: Some(500000),
            },
        }
    }

    fn banding(d: &Declaration, bands: Bands) -> Banding {
        Banding {
            field: d.amount.field.clone(),
            currency: d.amount.currency.clone(),
            currency_field: d.amount.currency_field.clone(),
            base: Risk::High,
            bands,
            floor: d.floor.clone(),
        }
    }

    fn body(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn the_lowest_band_an_amount_is_below_is_its_risk() {
        let d = charge();
        let b = banding(&d, d.bands.clone());
        assert_eq!(b.assess(&body(json!({"amount_pence": 4999}))).0, Risk::Low);
        assert_eq!(
            b.assess(&body(json!({"amount_pence": 5000}))).0,
            Risk::Medium
        );
        assert_eq!(
            b.assess(&body(json!({"amount_pence": 19999}))).0,
            Risk::Medium
        );
        // Above every band, the base risk.
        assert_eq!(
            b.assess(&body(json!({"amount_pence": 20000}))).0,
            Risk::High
        );
        assert_eq!(b.lowest(), Risk::Low);
    }

    #[test]
    fn an_amount_nobody_can_trust_is_the_base_risk() {
        let d = charge();
        let b = banding(&d, d.bands.clone());
        for untrusted in [
            json!({"amount_pence": "100"}),
            json!({"amount_pence": 100.0}),
            json!({"amount_pence": -100}),
            json!({"amount_pence": 18446744073709551615u64}),
            json!({"amount_pence": null}),
            json!({}),
        ] {
            assert_eq!(
                b.assess(&body(untrusted.clone())),
                (Risk::High, None),
                "{untrusted}"
            );
        }
    }

    #[test]
    fn the_floor_wins_whatever_a_band_says() {
        let d = charge();
        // Bands the check would refuse, as a turn might carry them were it ever
        // to: the floor still holds.
        let b = banding(
            &d,
            Bands {
                low: Some(10_000_000),
                medium: None,
            },
        );
        assert_eq!(
            b.assess(&body(json!({"amount_pence": 499_999}))).0,
            Risk::Low
        );
        assert_eq!(
            b.assess(&body(json!({"amount_pence": 500_000}))).0,
            Risk::High
        );
        // A floor above the base raises it too.
        let raised = Banding {
            base: Risk::Medium,
            bands: Bands::default(),
            floor: Floor {
                medium: None,
                high: Some(100),
            },
            ..b
        };
        assert_eq!(
            raised.assess(&body(json!({"amount_pence": 100}))).0,
            Risk::High
        );
        assert_eq!(
            raised.assess(&body(json!({"amount_pence": 99}))).0,
            Risk::Medium
        );
    }

    #[test]
    fn an_amount_in_another_currency_is_the_base_risk() {
        let mut d = charge();
        d.amount.currency_field = Some("currency".into());
        let b = banding(&d, d.bands.clone());
        assert_eq!(
            b.assess(&body(json!({"amount_pence": 100, "currency": "GBP"}))),
            (Risk::Low, Some(100))
        );
        for other in [
            json!({"amount_pence": 100, "currency": "JPY"}),
            json!({"amount_pence": 100, "currency": "gbp"}),
            json!({"amount_pence": 100, "currency": " GBP"}),
            json!({"amount_pence": 100, "currency": null}),
            json!({"amount_pence": 100}),
        ] {
            assert_eq!(
                b.assess(&body(other.clone())),
                (Risk::High, None),
                "{other}"
            );
        }
    }

    #[test]
    fn a_currency_field_is_one_the_approval_binds() {
        let binds = vec!["amount_pence".to_string(), "currency".to_string()];
        let numbers = vec!["amount_pence".to_string()];
        let mut d = charge();
        d.amount.currency_field = Some("currency".into());
        assert!(d.check(Risk::High, &binds, &numbers).is_ok());
        assert!(d.check(Risk::High, &numbers, &numbers).is_err(), "unbound");
        d.amount.currency_field = Some("amount_pence".into());
        assert!(d.check(Risk::High, &binds, &numbers).is_err(), "the amount");
    }

    #[test]
    fn a_band_at_or_above_the_base_means_nothing() {
        // A band for medium on a medium operation, were one ever committed,
        // would say medium of everything below it: the base already says so.
        let b = Banding {
            field: "n".into(),
            currency: "GBP".into(),
            currency_field: None,
            base: Risk::Low,
            bands: Bands {
                low: None,
                medium: Some(100),
            },
            floor: Floor::default(),
        };
        assert_eq!(b.assess(&body(json!({"n": 1}))).0, Risk::Low);
    }

    #[test]
    fn a_declaration_is_checked_against_its_gate() {
        let binds = vec!["booking_id".to_string(), "amount_pence".to_string()];
        let numbers = vec!["amount_pence".to_string()];
        assert!(charge().check(Risk::High, &binds, &numbers).is_ok());
        // Not a number a comparison may read.
        assert!(charge().check(Risk::High, &binds, &[]).is_err());
        assert!(charge().check(Risk::High, &numbers, &[]).is_err());
        // A medium band on a medium operation.
        assert!(charge().check(Risk::Medium, &binds, &numbers).is_err());

        let mut bad = charge();
        bad.amount.currency = "£".into();
        assert!(bad.check(Risk::High, &binds, &numbers).is_err());
        let mut bad = charge();
        bad.amount.minor_units = 9;
        assert!(bad.check(Risk::High, &binds, &numbers).is_err());
        let mut bad = charge();
        bad.bands.low = Some(30000);
        assert!(bad.check(Risk::High, &binds, &numbers).is_err(), "falling");
        let mut bad = charge();
        bad.floor.high = Some(10000);
        assert!(
            bad.check(Risk::High, &binds, &numbers).is_err(),
            "past the floor"
        );
        let mut bad = charge();
        bad.floor.medium = Some(900000);
        assert!(
            bad.check(Risk::High, &binds, &numbers).is_err(),
            "floors falling"
        );
    }

    #[test]
    fn a_workspace_moves_a_band_either_way_but_not_past_the_floor() {
        let d = charge();
        let lower = Bands {
            low: Some(1000),
            medium: Some(2000),
        };
        let higher = Bands {
            low: Some(50000),
            medium: Some(500000),
        };
        assert!(lower.check(Risk::High, &d.floor).is_ok());
        assert!(
            higher.check(Risk::High, &d.floor).is_ok(),
            "up to the floor"
        );
        let past = Bands {
            low: Some(1_000_000),
            medium: None,
        };
        assert!(past.check(Risk::High, &d.floor).is_err());
    }

    #[test]
    fn money_is_written_as_money() {
        assert_eq!(format(5000, "GBP", 2), "£50.00");
        assert_eq!(format(125000, "USD", 2), "$1,250.00");
        assert_eq!(format(1_000_000_000, "GBP", 2), "£10,000,000.00");
        assert_eq!(format(1250, "CHF", 2), "CHF 12.50");
        assert_eq!(format(500, "JPY", 0), "¥500");
        assert_eq!(format(5, "GBP", 2), "£0.05");
        assert_eq!(format(-5, "GBP", 2), "-£0.05");
    }
}
