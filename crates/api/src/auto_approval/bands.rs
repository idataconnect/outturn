//! A workspace's own bands: where a request's amount falls between low, medium
//! and its operation's base risk.
//!
//! See `docs/auto-approval.md`, "Risk from the amount". The operator declares
//! default bands with the skill; a workspace may set its own for every operation
//! in one currency, or for one operation, moved either way from the defaults but
//! never past the operator's floor. The operation's own row wins over its
//! currency's, and either over the defaults. What a turn runs under is resolved
//! here when the turn is prepared and committed into its token beside the
//! policies, so the gateway decides from what is signed.
//!
//! Who: as for a policy. A band is half of what a risk selector lets through,
//! so writing one takes `approvals:delegate` and `approvals:answer`, from a
//! person's own session; reading takes either.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use uuid::Uuid;

use outturn_core::egress::amount::{self, Bands, Floor};
use outturn_core::egress::policy::Risk;

use super::Declared;
use crate::router::{ApiError, ApiState, authenticate};

/// One row a workspace set.
#[derive(Debug, Clone, Serialize)]
pub struct Band {
    pub id: Uuid,
    pub currency: String,
    /// All three, for one operation; none, for every operation in the currency.
    pub skill_id: Option<Uuid>,
    pub requires: Option<String>,
    pub matches: Option<String>,
    pub low: Option<i64>,
    pub medium: Option<i64>,
    pub updated_by: Option<Uuid>,
    pub updated_by_name: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl Band {
    fn bands(&self) -> Bands {
        Bands {
            low: self.low,
            medium: self.medium,
        }
    }

    fn names(&self, d: &Declared) -> bool {
        self.skill_id == Some(d.skill_id)
            && self.requires.as_deref() == Some(d.gate.requires.as_str())
            && self
                .matches
                .as_deref()
                .and_then(super::normalize_matches)
                .is_some_and(|m| m == d.operation())
    }
}

/// Every row a workspace set.
pub async fn of(pool: &sqlx::PgPool, workspace_id: Uuid) -> Result<Vec<Band>, sqlx::Error> {
    let rows = sqlx::query(
        "select b.id, b.currency, b.skill_id, b.requires, b.matches, b.low, b.medium, \
                b.updated_by, u.display_name as updated_by_name, b.updated_at \
         from approval_bands b left join users u on u.id = b.updated_by \
         where b.workspace_id = $1 order by b.currency, b.id",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Band {
            id: r.get("id"),
            currency: r.get("currency"),
            skill_id: r.get("skill_id"),
            requires: r.get("requires"),
            matches: r.get("matches"),
            low: r.get("low"),
            medium: r.get("medium"),
            updated_by: r.get("updated_by"),
            updated_by_name: r.get("updated_by_name"),
            updated_at: r.get("updated_at"),
        })
        .collect())
}

/// Where an operation's bands come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The workspace's row for this operation.
    Operation,
    /// The workspace's row for every operation in its currency.
    Currency,
    /// What the operator declared with the skill.
    Operator,
}

/// The bands one money operation runs under in a workspace, and where they
/// came from. `None` for an operation that declares no amount.
pub fn effective(d: &Declared, rows: &[Band]) -> Option<(Bands, Source)> {
    let declared = d.amount.as_ref()?;
    let currency = &declared.amount.currency;
    let in_currency = rows.iter().filter(|r| &r.currency == currency);
    if let Some(own) = in_currency.clone().find(|r| r.names(d)) {
        return Some((own.bands(), Source::Operation));
    }
    // A currency's row is in the currency's standard units, the operation's own
    // in the operation's.
    if let Some(all) = in_currency.clone().find(|r| r.skill_id.is_none()) {
        return Some((
            all.bands()
                .from_standard(currency, declared.amount.minor_units),
            Source::Currency,
        ));
    }
    Some((declared.bands.clone(), Source::Operator))
}

/// Puts a workspace's bands on what its turn declares. The floor is the
/// operator's whatever a row says, and wins again when a request is assessed.
pub fn apply(declared: &mut [Declared], rows: &[Band]) {
    for d in declared.iter_mut() {
        let Some((bands, _)) = effective(d, rows) else {
            continue;
        };
        if let Some(banding) = d.banding.as_mut() {
            banding.bands = bands;
        }
    }
}

/// An amount as the API hands it to a page: the number, what it is in, and how
/// a person reads it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Money {
    pub amount: i64,
    pub currency: String,
    pub minor_units: u8,
    /// `£50.00`.
    pub display: String,
}

impl Money {
    pub fn of(amount: i64, currency: &str, minor_units: u8) -> Self {
        Self {
            amount,
            currency: currency.to_string(),
            minor_units,
            display: amount::format(amount, currency, minor_units),
        }
    }
}

/// Bands, each as money.
#[derive(Debug, Clone, Serialize)]
pub struct BandsView {
    pub low: Option<Money>,
    pub medium: Option<Money>,
}

/// A floor, each as money.
#[derive(Debug, Clone, Serialize)]
pub struct FloorView {
    pub medium: Option<Money>,
    pub high: Option<Money>,
}

fn bands_view(b: &Bands, currency: &str, minor_units: u8) -> BandsView {
    let m = |v: Option<i64>| v.map(|v| Money::of(v, currency, minor_units));
    BandsView {
        low: m(b.low),
        medium: m(b.medium),
    }
}

fn floor_view(f: &Floor, currency: &str, minor_units: u8) -> FloorView {
    let m = |v: Option<i64>| v.map(|v| Money::of(v, currency, minor_units));
    FloorView {
        medium: m(f.medium),
        high: m(f.high),
    }
}

/// One money operation the workspace's agents can reach, as the bands page
/// reads it.
#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    pub skill_id: Uuid,
    pub skill_name: Option<String>,
    pub requires: String,
    /// `POST /charges`.
    pub matches: String,
    pub field: String,
    pub currency: String,
    pub minor_units: u8,
    /// Its base risk: what an amount above every band, or one nobody can trust,
    /// works out to.
    pub risk: Risk,
    pub defaults: BandsView,
    pub floor: FloorView,
    pub effective: BandsView,
    pub source: Source,
    /// The workspace's row for this operation, where it set one.
    pub band_id: Option<Uuid>,
    /// Whether a workspace row reaches past this operation's floor, which then
    /// holds whatever the row says. Only a currency's row can: one for the
    /// operation is refused when it would.
    pub held_at_floor: bool,
}

/// One currency some money operation is in, with the workspace's row for it.
#[derive(Debug, Clone, Serialize)]
pub struct Currency {
    pub currency: String,
    /// The currency's standard digits after the point, which its row is in
    /// whatever its operations count in: `amount::standard_minor_units`.
    pub minor_units: u8,
    pub band: Option<Band>,
    pub effective: Option<BandsView>,
}

#[derive(Debug, Serialize)]
pub struct BandsPage {
    pub operations: Vec<Operation>,
    pub currencies: Vec<Currency>,
    /// Rows naming an operation no agent reaches now, kept rather than dropped
    /// so binding the skill again finds them.
    pub unused: Vec<Band>,
    /// Whether the caller may write here.
    pub may_write: bool,
}

/// Every money operation any of the workspace's agents can reach, once each,
/// with the name of its skill.
pub(super) async fn operations(
    state: &ApiState,
    workspace_id: Uuid,
) -> Result<Vec<(Declared, Option<String>)>, sqlx::Error> {
    use crate::skill::SkillStore as _;
    let agents: Vec<Uuid> =
        sqlx::query_scalar("select id from agents where workspace_id = $1 order by id")
            .bind(workspace_id)
            .fetch_all(&state.pool)
            .await?;
    let skills = crate::skill::PostgresSkillStore::new(state.pool.clone());
    let mut seen: BTreeMap<(Uuid, String, String), (Declared, Option<String>)> = BTreeMap::new();
    for agent in agents {
        let Ok(resolved) = skills.resolve_for_agent(workspace_id, agent).await else {
            continue;
        };
        let names: BTreeMap<Uuid, String> = resolved
            .iter()
            .map(|s| (s.skill_id, s.name.clone()))
            .collect();
        for d in super::declared_for(&state.pool, &resolved).await? {
            if d.amount.is_none() {
                continue;
            }
            let key = (d.skill_id, d.gate.requires.clone(), d.operation());
            let name = names.get(&d.skill_id).cloned();
            seen.entry(key).or_insert((d, name));
        }
    }
    Ok(seen.into_values().collect())
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn invalid(why: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, why.into())
}

fn page(ops: &[(Declared, Option<String>)], rows: &[Band], may_write: bool) -> BandsPage {
    let mut currencies: BTreeMap<String, u8> = BTreeMap::new();
    let mut operations = Vec::new();
    for (d, name) in ops {
        let Some(declared) = d.amount.as_ref() else {
            continue;
        };
        let (currency, minor) = (&declared.amount.currency, declared.amount.minor_units);
        currencies
            .entry(currency.clone())
            .or_insert(amount::standard_minor_units(currency));
        let Some((bands, source)) = effective(d, rows) else {
            continue;
        };
        let band_id = rows.iter().find(|r| r.names(d)).map(|r| r.id);
        operations.push(Operation {
            skill_id: d.skill_id,
            skill_name: name.clone(),
            requires: d.gate.requires.clone(),
            matches: d.operation(),
            field: declared.amount.field.clone(),
            currency: currency.clone(),
            minor_units: minor,
            risk: d.risk,
            defaults: bands_view(&declared.bands, currency, minor),
            floor: floor_view(&declared.floor, currency, minor),
            held_at_floor: source != Source::Operator
                && bands.check(d.risk, &declared.floor).is_err(),
            effective: bands_view(&bands, currency, minor),
            source,
            band_id,
        });
    }
    let currencies = currencies
        .into_iter()
        .map(|(currency, minor_units)| {
            let band = rows
                .iter()
                .find(|r| r.currency == currency && r.skill_id.is_none())
                .cloned();
            Currency {
                effective: band
                    .as_ref()
                    .map(|b| bands_view(&b.bands(), &currency, minor_units)),
                currency,
                minor_units,
                band,
            }
        })
        .collect();
    let unused = rows
        .iter()
        .filter(|r| r.skill_id.is_some() && !ops.iter().any(|(d, _)| r.names(d)))
        .cloned()
        .collect();
    BandsPage {
        operations,
        currencies,
        unused,
        may_write,
    }
}

/// `GET /v1/approval-bands`: every money operation the workspace's agents can
/// reach, the bands each runs under and where they came from, and the
/// workspace's rows.
pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<BandsPage>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_read(&state, &claims).await?;
    let may_write = super::api::may_write(&state, &claims).await.is_ok();
    let ops = operations(&state, claims.workspace_id)
        .await
        .map_err(internal)?;
    let rows = of(&state.pool, claims.workspace_id)
        .await
        .map_err(internal)?;
    Ok(Json(page(&ops, &rows, may_write)))
}

/// What a person writes: bands for a currency, or for one operation named as a
/// policy names one. A level left out has no band, so nothing works out to it.
#[derive(Debug, Deserialize)]
pub struct WriteBands {
    /// Required for a currency's row; for an operation's, its declared currency
    /// is used, and any other refused.
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub skill_id: Option<Uuid>,
    #[serde(default)]
    pub requires: Option<String>,
    #[serde(default)]
    pub matches: Option<String>,
    #[serde(default)]
    pub low: Option<i64>,
    #[serde(default)]
    pub medium: Option<i64>,
}

/// What a write says, checked against what it is about.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Checked {
    currency: String,
    operation: Option<(Uuid, String, String)>,
    bands: Bands,
}

pub(super) fn check(
    input: WriteBands,
    ops: &[(Declared, Option<String>)],
) -> Result<Checked, ApiError> {
    let bands = Bands {
        low: input.low,
        medium: input.medium,
    };
    if bands.is_empty() {
        return Err(invalid(
            "bands need a low or a medium; to go back to the defaults, delete the row",
        ));
    }
    let word = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let currency = word(input.currency);
    match (input.skill_id, word(input.requires), word(input.matches)) {
        (None, None, None) => {
            let currency =
                currency.ok_or_else(|| invalid("bands for a currency need the currency"))?;
            amount::check_currency(&currency).map_err(invalid)?;
            // Rising and above zero on their own; and never past the floor of an
            // operation in this currency, each at its own base risk.
            bands
                .check(Risk::High, &Floor::default())
                .map_err(invalid)?;
            for (d, name) in ops {
                let Some(declared) = d.amount.as_ref() else {
                    continue;
                };
                if declared.amount.currency != currency {
                    continue;
                }
                // In the operation's own units, as it will be applied.
                let own = bands.from_standard(&currency, declared.amount.minor_units);
                let at_base = Bands {
                    low: own.low.filter(|_| Risk::Low < d.risk),
                    medium: own.medium.filter(|_| Risk::Medium < d.risk),
                };
                at_base.check(d.risk, &declared.floor).map_err(|why| {
                    invalid(format!(
                        "{why} for {} in {}",
                        d.operation(),
                        name.as_deref().unwrap_or("a skill")
                    ))
                })?;
            }
            Ok(Checked {
                currency,
                operation: None,
                bands,
            })
        }
        (Some(skill_id), Some(requires), Some(matches)) => {
            let matches = super::normalize_matches(&matches)
                .ok_or_else(|| invalid("matches names an operation, e.g. `POST /charges`"))?;
            let (d, _) = ops
                .iter()
                .find(|(d, _)| {
                    d.skill_id == skill_id
                        && d.gate.requires == requires
                        && d.operation() == matches
                })
                .ok_or_else(|| {
                    invalid("no agent here reaches an operation like that with an amount")
                })?;
            let declared = d.amount.as_ref().expect("only money operations are listed");
            if currency
                .as_ref()
                .is_some_and(|c| *c != declared.amount.currency)
            {
                return Err(invalid(format!(
                    "that operation's amounts are in {}; bands are never converted",
                    declared.amount.currency
                )));
            }
            bands.check(d.risk, &declared.floor).map_err(invalid)?;
            Ok(Checked {
                currency: declared.amount.currency.clone(),
                operation: Some((skill_id, requires, matches)),
                bands,
            })
        }
        _ => Err(invalid(
            "an operation's bands name its skill_id, requires and matches together",
        )),
    }
}

/// Writes a checked row, replacing the one for the same currency or
/// operation in place, and says which it is.
pub(super) async fn write(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    subject: Uuid,
    checked: &Checked,
) -> Result<Uuid, sqlx::Error> {
    let (skill_id, requires, matches) = match checked.operation.clone() {
        Some((s, r, m)) => (Some(s), Some(r), Some(m)),
        None => (None, None, None),
    };

    // One row per currency or operation, by the partial unique indexes; the
    // existing one is replaced in place, keeping its id.
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select id from approval_bands \
         where workspace_id = $1 and currency = $2 \
           and skill_id is not distinct from $3 and requires is not distinct from $4 \
           and matches is not distinct from $5 for update",
    )
    .bind(workspace_id)
    .bind(&checked.currency)
    .bind(skill_id)
    .bind(&requires)
    .bind(&matches)
    .fetch_optional(&mut **tx)
    .await?;
    let id = match existing {
        Some(id) => {
            sqlx::query(
                "update approval_bands set low = $3, medium = $4, updated_by = $5, \
                        updated_at = now() \
                 where workspace_id = $1 and id = $2",
            )
            .bind(workspace_id)
            .bind(id)
            .bind(checked.bands.low)
            .bind(checked.bands.medium)
            .bind(subject)
            .execute(&mut **tx)
            .await?;
            id
        }
        None => {
            let id = Uuid::now_v7();
            sqlx::query(
                "insert into approval_bands \
                     (workspace_id, id, currency, skill_id, requires, matches, low, medium, \
                      updated_by) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(workspace_id)
            .bind(id)
            .bind(&checked.currency)
            .bind(skill_id)
            .bind(&requires)
            .bind(&matches)
            .bind(checked.bands.low)
            .bind(checked.bands.medium)
            .bind(subject)
            .execute(&mut **tx)
            .await?;
            id
        }
    };
    Ok(id)
}

/// The rows the one line writes: for each operation of this kind in this
/// currency, a low band at `low` -- given in the currency's standard units,
/// and rounded down into the operation's own, the side that asks more often --
/// keeping its medium bound where that is still above it. An operation whose
/// low band is that already is left as it is. Each row checked as a write of
/// it would be, so nothing is returned that the floor refuses.
pub(super) fn one_line(
    ops: &[(Declared, Option<String>)],
    rows: &[Band],
    requires: &str,
    currency: &str,
    low: i64,
) -> Result<Vec<Checked>, ApiError> {
    if low <= 0 {
        return Err(invalid(format!(
            "the amount requests go through under is a figure in {currency} above zero"
        )));
    }
    let of_kind: Vec<&Declared> = ops
        .iter()
        .map(|(d, _)| d)
        .filter(|d| d.gate.requires == requires)
        .filter(|d| {
            d.amount
                .as_ref()
                .is_some_and(|a| a.amount.currency == currency)
        })
        .collect();
    if of_kind.is_empty() {
        return Err(invalid(format!(
            "no agent here reaches an operation of that kind with an amount in {currency}"
        )));
    }
    let mut writes = Vec::new();
    for d in of_kind {
        let declared = d.amount.as_ref().expect("filtered on its amount");
        let own =
            amount::from_standard(low, currency, declared.amount.minor_units).ok_or_else(|| {
                invalid(format!(
                    "{} counts in coarser units than that figure says",
                    d.operation()
                ))
            })?;
        let Some((now, _)) = effective(d, rows) else {
            continue;
        };
        if now.low == Some(own) {
            continue;
        }
        writes.push(check(
            WriteBands {
                currency: None,
                skill_id: Some(d.skill_id),
                requires: Some(d.gate.requires.clone()),
                matches: Some(d.operation()),
                low: Some(own),
                medium: now.medium.filter(|m| *m > own),
            },
            ops,
        )?);
    }
    Ok(writes)
}

/// `PUT /v1/approval-bands`: sets a workspace's bands for a currency or an
/// operation, replacing any it had. Takes effect from the next turn: a running
/// one keeps what its token committed to, as with every other rule.
pub async fn put(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<WriteBands>,
) -> Result<Json<Band>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_write(&state, &claims).await?;
    let ops = operations(&state, claims.workspace_id)
        .await
        .map_err(internal)?;
    let checked = check(input, &ops)?;
    let mut tx = state.pool.begin().await.map_err(internal)?;
    let id = write(&mut tx, claims.workspace_id, claims.subject, &checked)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    tracing::info!(
        workspace_id = %claims.workspace_id,
        band_id = %id,
        currency = %checked.currency,
        set_by = %claims.subject,
        "a workspace's approval bands were set"
    );
    // Bands move what a broad policy reaches, so what waits on one may change.
    super::waiting::reconcile(&state, claims.workspace_id).await;
    let row = of(&state.pool, claims.workspace_id)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| internal("the row just written is gone"))?;
    Ok(Json(row))
}

/// `DELETE /v1/approval-bands/{id}`: back to what stood beneath it.
pub async fn delete(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_write(&state, &claims).await?;
    let gone = sqlx::query("delete from approval_bands where workspace_id = $1 and id = $2")
        .bind(claims.workspace_id)
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(internal)?
        .rows_affected();
    if gone == 0 {
        return Err((StatusCode::NOT_FOUND, "no such bands".to_string()));
    }
    tracing::info!(
        workspace_id = %claims.workspace_id,
        band_id = %id,
        by = %claims.subject,
        "a workspace's approval bands were removed"
    );
    super::waiting::reconcile(&state, claims.workspace_id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::super::tests::declared;
    use super::*;
    use outturn_core::egress::amount::{Amount, Banding, Declaration};

    fn money(d: &mut Declared) {
        let declaration = Declaration {
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
        };
        d.banding = Some(Banding {
            field: "amount_pence".into(),
            currency: "GBP".into(),
            currency_field: None,
            minor_units: 2,
            base: d.risk,
            bands: declaration.bands.clone(),
            floor: declaration.floor.clone(),
        });
        d.amount = Some(declaration);
    }

    fn row(currency: &str, operation: Option<&Declared>, low: i64) -> Band {
        Band {
            id: Uuid::now_v7(),
            currency: currency.into(),
            skill_id: operation.map(|d| d.skill_id),
            requires: operation.map(|d| d.gate.requires.clone()),
            matches: operation.map(|d| d.operation().to_lowercase()),
            low: Some(low),
            medium: None,
            updated_by: None,
            updated_by_name: None,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn an_operations_row_wins_over_its_currencys_and_either_over_the_defaults() {
        let mut d = declared("charge", Risk::High);
        money(&mut d);
        assert_eq!(effective(&d, &[]).unwrap().1, Source::Operator);
        let currency = row("GBP", None, 1000);
        let dollars = row("USD", None, 9);
        assert_eq!(
            effective(&d, &[currency.clone(), dollars.clone()]).unwrap(),
            (
                Bands {
                    low: Some(1000),
                    medium: None
                },
                Source::Currency
            )
        );
        let own = row("GBP", Some(&d), 2000);
        let (bands, source) = effective(&d, &[currency, own, dollars]).unwrap();
        assert_eq!((bands.low, source), (Some(2000), Source::Operation));

        // Never converted: only dollars set, and the pounds are the defaults.
        let mut only_dollars = vec![d.clone()];
        apply(&mut only_dollars, &[row("USD", None, 9)]);
        assert_eq!(
            only_dollars[0].banding.as_ref().unwrap().bands.low,
            Some(5000)
        );

        // An operation with no amount has no bands to set.
        assert!(effective(&declared("delete", Risk::High), &[]).is_none());
    }

    /// The same charge, counted in whole pounds rather than pence.
    fn in_whole_pounds(d: &mut Declared) {
        money(d);
        let declared = d.amount.as_mut().unwrap();
        declared.amount.field = "amount_gbp".into();
        declared.amount.minor_units = 0;
        declared.bands = Bands {
            low: Some(50),
            medium: Some(200),
        };
        declared.floor = Floor {
            medium: None,
            high: Some(5000),
        };
        let banding = d.banding.as_mut().unwrap();
        banding.field = "amount_gbp".into();
        banding.minor_units = 0;
        banding.bands = declared.bands.clone();
        banding.floor = declared.floor.clone();
    }

    #[test]
    fn one_pounds_row_bands_a_pence_operation_and_a_whole_pound_one_alike() {
        let mut pence = declared("charge", Risk::High);
        money(&mut pence);
        let mut pounds = declared("refund", Risk::High);
        in_whole_pounds(&mut pounds);
        // £30.00, in pence as every currency row is.
        let gbp = row("GBP", None, 3000);
        let mut both = vec![pence.clone(), pounds.clone()];
        apply(&mut both, std::slice::from_ref(&gbp));
        assert_eq!(both[0].banding.as_ref().unwrap().bands.low, Some(3000));
        assert_eq!(both[1].banding.as_ref().unwrap().bands.low, Some(30));

        // £30.50 is no whole number of pounds: the pound operation is held to
        // £30, the side that asks more often.
        let gbp = row("GBP", None, 3050);
        let mut both = vec![pence.clone(), pounds.clone()];
        apply(&mut both, &[gbp]);
        assert_eq!(both[0].banding.as_ref().unwrap().bands.low, Some(3050));
        assert_eq!(both[1].banding.as_ref().unwrap().bands.low, Some(30));

        // An operation's own row stays in its own units.
        let own = row("GBP", Some(&pounds), 40);
        assert_eq!(effective(&pounds, &[own]).unwrap().0.low, Some(40));

        // The page shows the currency's row in pence whatever its operations
        // count in, and each operation's effective bands in its own.
        let ops = vec![(pence, None), (pounds, None)];
        let page = page(&ops, &[row("GBP", None, 3000)], true);
        assert_eq!(page.currencies[0].minor_units, 2);
        let refund = page
            .operations
            .iter()
            .find(|o| o.requires == "refund")
            .unwrap();
        assert_eq!(refund.effective.low.as_ref().unwrap().display, "£30");
    }

    #[test]
    fn a_currency_row_is_checked_against_each_floor_in_its_own_units() {
        let mut pounds = declared("refund", Risk::High);
        in_whole_pounds(&mut pounds);
        let ops = vec![(pounds, None)];
        let write = |v: serde_json::Value| check(serde_json::from_value(v).unwrap(), &ops);
        // £4,000.00 in pence is under the £5,000 floor; £6,000.00 is past it.
        assert!(write(serde_json::json!({"currency": "GBP", "low": 400_000})).is_ok());
        assert!(write(serde_json::json!({"currency": "GBP", "low": 600_000})).is_err());
    }

    #[test]
    fn the_one_line_sets_each_operations_low_band_in_its_own_units() {
        let mut pence = declared("charge", Risk::High);
        money(&mut pence);
        let mut pounds = declared("charge", Risk::High);
        in_whole_pounds(&mut pounds);
        pounds.gate.path = "/payments".into();
        let ops = vec![(pence.clone(), None), (pounds.clone(), None)];
        // £40.50: pence take it as it is, whole pounds round down to £40.
        let writes = one_line(&ops, &[], "charge", "GBP", 4050).unwrap();
        let lows: Vec<_> = writes
            .iter()
            .map(|w| (w.bands.low, w.bands.medium))
            .collect();
        assert_eq!(lows, vec![(Some(4050), Some(20000)), (Some(40), Some(200))]);
        // Where a low band is that already, nothing is written for it.
        assert_eq!(one_line(&ops, &[], "charge", "GBP", 5000).unwrap().len(), 0);
        // A medium the new low passes is dropped rather than left below it.
        let writes = one_line(&ops, &[], "charge", "GBP", 30000).unwrap();
        assert_eq!(writes[0].bands.medium, None);
        // Past a floor, of another kind, or in another currency: refused.
        assert!(one_line(&ops, &[], "charge", "GBP", 900_000).is_err());
        assert!(one_line(&ops, &[], "refund", "GBP", 4000).is_err());
        assert!(one_line(&ops, &[], "charge", "USD", 4000).is_err());
        assert!(one_line(&ops, &[], "charge", "GBP", 0).is_err());
        // Less than one whole pound means nothing to a whole-pound operation.
        assert!(one_line(&ops, &[], "charge", "GBP", 50).is_err());
    }

    #[test]
    fn a_write_is_checked_against_the_operation_it_names_and_the_floor() {
        let mut d = declared("charge", Risk::High);
        money(&mut d);
        let ops = vec![(d.clone(), Some("Charging".to_string()))];
        let write = |v: serde_json::Value| check(serde_json::from_value(v).unwrap(), &ops);

        // Up or down from the defaults, either way.
        assert!(write(serde_json::json!({"currency": "GBP", "low": 50000})).is_ok());
        assert!(write(serde_json::json!({"currency": "GBP", "low": 100})).is_ok());
        let op = write(serde_json::json!({
            "skill_id": d.skill_id, "requires": "charge", "matches": "post /charges",
            "low": 1000, "medium": 2000,
        }))
        .unwrap();
        assert_eq!(op.currency, "GBP", "the operation's own currency");

        for bad in [
            // Past the floor, which says high from £5,000.
            serde_json::json!({"currency": "GBP", "low": 1_000_000}),
            serde_json::json!({
                "skill_id": d.skill_id, "requires": "charge", "matches": "POST /charges",
                "medium": 900_000,
            }),
            // Falling, empty, or not a currency.
            serde_json::json!({"currency": "GBP", "low": 3000, "medium": 2000}),
            serde_json::json!({"currency": "GBP"}),
            serde_json::json!({"currency": "pounds", "low": 10}),
            // Converted, half-named, or nothing an agent reaches.
            serde_json::json!({
                "currency": "USD", "skill_id": d.skill_id, "requires": "charge",
                "matches": "POST /charges", "low": 10,
            }),
            serde_json::json!({"skill_id": d.skill_id, "low": 10}),
            serde_json::json!({
                "skill_id": d.skill_id, "requires": "charge", "matches": "POST /refunds",
                "low": 10,
            }),
        ] {
            assert!(write(bad.clone()).is_err(), "{bad}");
        }
        // Dollars say nothing about a floor in pounds.
        assert!(write(serde_json::json!({"currency": "USD", "low": 1_000_000})).is_ok());
    }

    #[test]
    fn the_page_shows_money_as_money_and_says_when_the_floor_holds() {
        let mut d = declared("charge", Risk::High);
        money(&mut d);
        let ops = vec![(d.clone(), Some("Charging".to_string()))];
        // A currency row written before the floor was what it is now.
        let p = page(&ops, &[row("GBP", None, 1_000_000)], true);
        let op = &p.operations[0];
        assert_eq!(op.defaults.low.as_ref().unwrap().display, "£50.00");
        assert_eq!(op.floor.high.as_ref().unwrap().display, "£5,000.00");
        assert_eq!(op.source, Source::Currency);
        assert!(op.held_at_floor);
        assert_eq!(p.currencies[0].currency, "GBP");
        assert_eq!(p.currencies[0].minor_units, 2);
    }
}
