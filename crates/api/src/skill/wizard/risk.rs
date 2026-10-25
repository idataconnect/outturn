//! A guess at an operation's approval rule, for the operator to confirm.
//!
//! The heuristics read names and methods and nothing else, so they are cheap,
//! repeatable and wrong in ways a person can see at a glance. They are only
//! ever offered: a risk decides what goes out without anybody being asked, so
//! nothing here reaches a generated file unless the operator confirms it into
//! an `approval` annotation. See docs/auto-approval.md, "Where the wizard comes
//! in".

use outturn_core::egress::policy::Risk;

use super::parse::{BodyField, Operation};

/// What an operation would be gated as, and what its rule would say.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SuggestedApproval {
    /// `low`, `medium` or `high`: the rule's `risk` if the operator gates it.
    pub risk: Risk,
    /// Whether to gate it at all. A read is not; a delete or a payment is.
    pub gate: bool,
    /// The act word the rule asks for.
    pub requires: String,
    /// The gate's path as the specification writes it, named segments and
    /// all, so a rule can bind them; cut at the first parameter and starred
    /// where a parameter's name is not one a rule can say. The page puts the
    /// base URL's own path in front.
    pub matches_path: String,
    /// The path parameters and body fields the rule is about. A rule needs at
    /// least one, so an operation with neither cannot have one written.
    pub binds: Vec<String>,
    /// The bound fields that are numbers, which a policy may compare.
    pub numbers: Vec<String>,
    pub amount: Option<SuggestedAmount>,
    /// Why, in a phrase the page shows beside it.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SuggestedAmount {
    pub field: String,
    /// An ISO 4217 code, where the specification says one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// How many decimal places the amount's smallest unit is, where its name
    /// says it is in one (`_cents`, `_pence`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor_units: Option<u32>,
    /// The sibling field a request names its currency in, where there is one,
    /// so an amount in some other currency is not read against these bands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency_field: Option<String>,
}

/// Words that make a write dangerous whatever its method: money moving, or a
/// thing that cannot be taken back. Matched whole, with an ordinary ending.
const DANGEROUS: &[&str] = &[
    "pay",
    "payment",
    "charge",
    "refund",
    "transfer",
    "cancel",
    "delete",
    "destroy",
    "send",
    "payout",
    "withdraw",
    "withdrawal",
    "deposit",
    "void",
    "capture",
    "purchase",
    "disburse",
    "revoke",
    "terminate",
];

const CURRENCIES: &[&str] = &[
    "USD", "EUR", "GBP", "AUD", "CAD", "NZD", "JPY", "CHF", "SEK", "NOK", "DKK", "INR", "CNY",
    "HKD", "SGD", "MXN", "BRL", "ZAR", "PLN", "CZK", "KRW",
];

pub fn suggest(op: &Operation) -> SuggestedApproval {
    let method = op.method.to_ascii_uppercase();
    let read = matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS");
    let words = words_of(op);
    let danger = if read {
        None
    } else {
        // The first in reading order, so `refund_charge` is a refund.
        words
            .iter()
            .find_map(|t| DANGEROUS.iter().find(|w| is_form_of(t, w)))
            .copied()
    };

    let fields: &[BodyField] = op
        .request_body
        .as_ref()
        .filter(|b| b.is_json())
        .map(|b| b.fields.as_slice())
        .unwrap_or_default();
    let amount = if read {
        None
    } else {
        amount_field(fields).map(|f| SuggestedAmount {
            field: f.name.clone(),
            currency: currency_of(f, fields),
            minor_units: minor_units(&f.name),
            currency_field: currency_sibling(fields).map(|c| c.name.clone()),
        })
    };

    let (mut risk, mut gate, mut reason, requires) = match (read, method.as_str(), danger) {
        (true, _, _) => (Risk::Low, false, "reads change nothing".to_string(), None),
        (_, "DELETE", _) => (
            Risk::High,
            true,
            "a delete cannot be taken back".to_string(),
            Some("delete".to_string()),
        ),
        (_, _, Some(word)) => (
            Risk::High,
            true,
            format!("named for {word}, which moves money or cannot be undone"),
            Some(word.to_string()),
        ),
        // A request that carries an amount moves money whatever it is called.
        _ if amount.is_some() => (
            Risk::High,
            true,
            "it carries an amount of money".to_string(),
            None,
        ),
        _ => (
            Risk::Medium,
            false,
            "a create or update that can be corrected".to_string(),
            None,
        ),
    };

    // The specification's own word can raise a guess and never lower it: a
    // payout declared low is still named for a payout.
    if let Some((level, gated)) = op.risk_hint.as_ref().and_then(hint)
        && (level > risk || gated == Some(true))
    {
        if level > risk {
            risk = level;
            reason = "the specification declares x-risk".to_string();
        }
        gate = gate || gated.unwrap_or(level != Risk::Low);
    }

    let mut binds: Vec<String> = fields
        .iter()
        .filter(|f| f.required)
        .map(|f| f.name.clone())
        .collect();
    if let Some(a) = &amount
        && !binds.contains(&a.field)
    {
        binds.push(a.field.clone());
    }
    if binds.is_empty() {
        binds = fields.iter().map(|f| f.name.clone()).collect();
    }
    // Which invoice a delete is of is what a person deciding it needs to see,
    // and for `DELETE /invoices/{id}` it is all there is to bind. First, in
    // the order the path has them, since they name what the body is about.
    let path = path_params(&op.path);
    if let Some(path) = &path {
        let mut named: Vec<String> = path
            .iter()
            .filter(|p| !binds.contains(p))
            .cloned()
            .collect();
        named.append(&mut binds);
        binds = named;
    }
    let numbers = fields
        .iter()
        .filter(|f| matches!(f.schema.type_name.as_str(), "integer" | "number"))
        .filter(|f| binds.contains(&f.name))
        .map(|f| f.name.clone())
        .collect();

    SuggestedApproval {
        risk,
        gate,
        requires: requires.unwrap_or_else(|| op.name.clone()),
        matches_path: match path {
            Some(_) => op.path.clone(),
            None => gate_path(&op.path),
        },
        binds,
        numbers,
        amount,
        reason,
    }
}

/// `x-risk: high`, or `x-risk: { level: high, gated: false }`.
fn hint(v: &serde_json::Value) -> Option<(Risk, Option<bool>)> {
    let level = |s: &str| Risk::parse(&s.trim().to_ascii_lowercase());
    match v {
        serde_json::Value::String(s) => level(s).map(|l| (l, None)),
        serde_json::Value::Object(o) => {
            let l = level(o.get("level")?.as_str()?)?;
            Some((l, o.get("gated").and_then(|g| g.as_bool())))
        }
        _ => None,
    }
}

/// The lowercase words in an operation's name, path and summary. A path
/// parameter is not a word the operation is named for.
fn words_of(op: &Operation) -> Vec<String> {
    let path: String = op
        .path
        .split('/')
        .filter(|s| !s.contains('{'))
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!("{} {} {}", op.name, path, op.summary).replace(['_', '-', '.', ':'], " ");
    let mut words = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            prev_lower = false;
            continue;
        }
        // A camelCase boundary in a path segment or summary still splits.
        if c.is_uppercase() && prev_lower && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        prev_lower = c.is_lowercase();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a token is the word or an ordinary form of it: `charges`,
/// `refunded`, `charging`. Not `sender` for `send`.
pub(super) fn is_form_of(token: &str, word: &str) -> bool {
    let stem = word.strip_suffix('e').unwrap_or(word);
    token.strip_prefix(word).is_some_and(|rest| {
        matches!(
            rest,
            "" | "s" | "es" | "d" | "ed" | "ing" | "ment" | "ments"
        )
    }) || token
        .strip_prefix(stem)
        .is_some_and(|rest| matches!(rest, "ing" | "ed"))
}

/// The path's parameters, where every one is a name a rule can bind -- letters,
/// digits and underscores, as an operation's url takes them -- and `None` where
/// there are none or one is not.
fn path_params(path: &str) -> Option<Vec<String>> {
    let names: Vec<String> = path
        .split('/')
        .filter(|s| s.contains('{') || s.contains('}'))
        .map(|s| {
            s.strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .filter(|n| {
                    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
                .map(str::to_string)
        })
        .collect::<Option<_>>()?;
    let unique = names
        .iter()
        .enumerate()
        .all(|(i, n)| !names[..i].contains(n));
    (!names.is_empty() && unique).then_some(names)
}

/// The path a gate can say: whole, or up to the first parameter with a star.
fn gate_path(path: &str) -> String {
    match path.find('{') {
        Some(i) => format!("{}*", &path[..i]),
        None => path.to_string(),
    }
}

/// The body field most likely to be how much money a request is for: a
/// number named like an amount, preferring one that says it is in minor units.
fn amount_field(fields: &[BodyField]) -> Option<&BodyField> {
    let candidates: Vec<&BodyField> = fields
        .iter()
        .filter(|f| matches!(f.schema.type_name.as_str(), "integer" | "number"))
        .filter(|f| looks_like_amount(&f.name))
        .collect();
    candidates
        .iter()
        .find(|f| minor_units(&f.name).is_some())
        .or_else(|| candidates.iter().find(|f| f.required))
        .or(candidates.first())
        .copied()
}

fn looks_like_amount(name: &str) -> bool {
    let n = name.to_ascii_lowercase().replace('-', "_");
    if n.ends_with("_count") || n.starts_with("max_") || n.starts_with("min_") {
        return false;
    }
    minor_units(&n).is_some()
        || matches!(
            n.as_str(),
            "amount" | "total" | "price" | "cost" | "fee" | "grand_total"
        )
        || ["_amount", "_total", "_price"]
            .iter()
            .any(|s| n.ends_with(s))
        || n.starts_with("amount_")
}

/// Two places, for a name that says it is a count of the smallest unit.
fn minor_units(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    ["_cents", "_pence", "_pennies", "_minor", "_minor_units"]
        .iter()
        .any(|s| n.ends_with(s))
        .then_some(2)
}

/// The currency, where the specification says: a sibling `currency` field with
/// one value allowed or a default, else a code in the amount's own description.
fn currency_of(amount: &BodyField, fields: &[BodyField]) -> Option<String> {
    let known = |s: &str| {
        let s = s.trim().to_ascii_uppercase();
        CURRENCIES.contains(&s.as_str()).then_some(s)
    };
    if let Some(f) = currency_sibling(fields) {
        if let Some([only]) = f.schema.enum_values.as_deref()
            && let Some(c) = known(only)
        {
            return Some(c);
        }
        if let Some(c) = f.schema.default.as_deref().and_then(known) {
            return Some(c);
        }
        if let Some(c) = [f.schema.description.as_deref(), Some(&f.description)]
            .into_iter()
            .flatten()
            .find_map(code_in)
        {
            return Some(c);
        }
    }
    [
        amount.schema.description.as_deref(),
        Some(amount.description.as_str()),
    ]
    .into_iter()
    .flatten()
    .find_map(code_in)
    .or_else(|| amount.schema.format.as_deref().and_then(known))
    .or_else(|| {
        amount
            .name
            .to_ascii_lowercase()
            .ends_with("_pence")
            .then(|| "GBP".to_string())
    })
}

/// A field beside the amount named for its currency.
fn currency_sibling(fields: &[BodyField]) -> Option<&BodyField> {
    fields.iter().find(|f| {
        let n = f.name.to_ascii_lowercase();
        n == "currency" || n == "currency_code" || n.ends_with("_currency")
    })
}

/// A currency code written in prose, as its own upper-case word.
fn code_in(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphabetic())
        .find(|w| CURRENCIES.contains(w))
        .map(str::to_string)
}
