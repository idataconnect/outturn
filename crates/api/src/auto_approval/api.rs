//! The policy page's endpoints, and the count the approval prompt reads.
//!
//! Writing a policy takes `approvals:delegate` and `approvals:answer` together,
//! both checked when it is written and both recorded on it by who wrote it --
//! neither is re-checked on each use, for the same reason a grant is not. And a
//! person's own session: setting a policy is never done by an agent alone, so the
//! write endpoints refuse every token that is not a browser session's. An agent
//! may one day propose a policy; a person confirms it here.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use uuid::Uuid;

use outturn_core::auth::{Authority, SessionClaims};
use outturn_core::egress::policy::{self, Risk, Test};

use super::{DEFAULT_LIFETIME_DAYS, MAX_LIFETIME_DAYS, Policy};
use crate::Page;
use crate::router::{ApiError, ApiState, authenticate, authorities_of};

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn forbidden() -> ApiError {
    (
        StatusCode::FORBIDDEN,
        outturn_core::auth::AuthError::Forbidden.to_string(),
    )
}

/// Reading is for whoever answers approvals or sets policies: the rows say what
/// is waved through, which is what an approver most needs to know.
pub(super) async fn may_read(state: &ApiState, claims: &SessionClaims) -> Result<(), ApiError> {
    let held = authorities_of(state, claims).await?;
    if held.contains(&Authority::ApprovalsAnswer) || held.contains(&Authority::ApprovalsDelegate) {
        Ok(())
    } else {
        Err(forbidden())
    }
}

/// Writing is for a person holding both, in their own session.
pub(super) async fn may_write(state: &ApiState, claims: &SessionClaims) -> Result<(), ApiError> {
    // A browser session is the only token carrying a refresh family. A turn's
    // token is for the gateway and is refused here anyway; this also refuses
    // anything else that is not somebody signed in.
    if claims.family_id.is_none() {
        return Err((
            StatusCode::FORBIDDEN,
            "a policy is set by a person, from their own session".to_string(),
        ));
    }
    let held = authorities_of(state, claims).await?;
    if held.contains(&Authority::ApprovalsDelegate) && held.contains(&Authority::ApprovalsAnswer) {
        Ok(())
    } else {
        Err(forbidden())
    }
}

/// A policy as the page reads it: the row, and what it is now.
#[derive(Debug, Serialize)]
pub struct PolicyView {
    #[serde(flatten)]
    pub policy: Policy,
    pub state: &'static str,
    /// Each daily total as money, so the page writes none itself.
    pub daily_totals_money: BTreeMap<String, super::bands::Money>,
}

fn view(policy: Policy) -> PolicyView {
    let state = policy.state(Utc::now());
    let daily_totals_money = policy
        .daily_totals
        .iter()
        .map(|(c, v)| (c.clone(), super::bands::Money::standard(*v, c)))
        .collect();
    PolicyView {
        policy,
        state,
        daily_totals_money,
    }
}

/// What a person writes. The same shape for a new policy and an edit.
#[derive(Debug, Deserialize)]
pub struct WritePolicy {
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    #[serde(default = "include")]
    pub effect: String,
    pub selector: String,
    #[serde(default)]
    pub max_risk: Option<String>,
    #[serde(default)]
    pub requires: Option<String>,
    #[serde(default)]
    pub skill_id: Option<Uuid>,
    #[serde(default)]
    pub matches: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Test>,
    #[serde(default)]
    pub per_turn: Option<i32>,
    #[serde(default)]
    pub per_day: Option<i32>,
    #[serde(default)]
    pub sum_field: Option<String>,
    #[serde(default)]
    pub sum_per_day: Option<i64>,
    /// The most of each currency it lets out in a day, in minor units:
    /// `{"GBP": 50000}`.
    #[serde(default)]
    pub daily_totals: BTreeMap<String, i64>,
    #[serde(default = "trial")]
    pub mode: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Absent: ninety days from now.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

fn include() -> String {
    "include".into()
}

fn trial() -> String {
    "trial".into()
}

/// A write, checked: everything a row may say, and nothing it may not.
struct Checked {
    agent_id: Option<Uuid>,
    effect: &'static str,
    selector: &'static str,
    max_risk: Option<String>,
    requires: Option<String>,
    skill_id: Option<Uuid>,
    matches: Option<String>,
    conditions: Vec<Test>,
    per_turn: i32,
    per_day: i32,
    sum_field: Option<String>,
    sum_per_day: Option<i64>,
    daily_totals: BTreeMap<String, i64>,
    mode: &'static str,
    note: Option<String>,
    expires_at: DateTime<Utc>,
}

fn invalid(why: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, why.into())
}

fn check(input: WritePolicy, now: DateTime<Utc>) -> Result<Checked, ApiError> {
    let effect = match input.effect.as_str() {
        "include" => "include",
        "exclude" => "exclude",
        _ => return Err(invalid("effect is include or exclude")),
    };
    let word = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let requires = word(input.requires);
    // A bound on the risk a request works out to: required of a risk selector,
    // and for the others "charges, up to low".
    let bound = match input.max_risk.as_deref() {
        None => None,
        Some(raw) => Some(
            Risk::parse(raw)
                .ok_or_else(|| invalid("max_risk is low, medium or high"))?
                .as_str()
                .to_string(),
        ),
    };
    let (selector, max_risk, requires, skill_id, matches) = match input.selector.as_str() {
        "risk" => {
            let risk = input
                .max_risk
                .as_deref()
                .and_then(Risk::parse)
                .ok_or_else(|| invalid("a risk selector needs max_risk: low, medium or high"))?;
            ("risk", Some(risk.as_str().to_string()), None, None, None)
        }
        "act" => {
            let requires = requires.ok_or_else(|| invalid("an act selector needs requires"))?;
            ("act", bound, Some(requires), None, None)
        }
        "operation" => {
            let requires =
                requires.ok_or_else(|| invalid("an operation selector needs requires"))?;
            let skill_id = input
                .skill_id
                .ok_or_else(|| invalid("an operation selector needs skill_id"))?;
            let matches = input
                .matches
                .as_deref()
                .and_then(super::normalize_matches)
                .ok_or_else(|| {
                    invalid("an operation selector needs matches, e.g. `POST /charges`")
                })?;
            (
                "operation",
                bound,
                Some(requires),
                Some(skill_id),
                Some(matches),
            )
        }
        _ => return Err(invalid("selector is risk, act or operation")),
    };

    policy::check_tests(&input.conditions).map_err(invalid)?;
    for (currency, total) in &input.daily_totals {
        outturn_core::egress::amount::check_currency(currency).map_err(invalid)?;
        if *total < 0 {
            return Err(invalid(format!(
                "the daily total in {currency} is an amount at or above zero"
            )));
        }
    }

    let expires_at = input
        .expires_at
        .unwrap_or(now + chrono::Duration::days(DEFAULT_LIFETIME_DAYS));
    if expires_at <= now {
        return Err(invalid("a policy's expiry has to be in the future"));
    }
    if expires_at > now + chrono::Duration::days(MAX_LIFETIME_DAYS) {
        return Err(invalid(format!(
            "a policy may stand for at most {MAX_LIFETIME_DAYS} days; renew it when it runs out"
        )));
    }

    // An exclusion stops requests rather than letting them through, so it has
    // nothing to test and nothing to cap, and it applies from the moment it is
    // written.
    if effect == "exclude" {
        if !input.conditions.is_empty()
            || input.sum_field.is_some()
            || !input.daily_totals.is_empty()
        {
            return Err(invalid(
                "an exclusion covers its whole selector, with no tests",
            ));
        }
        return Ok(Checked {
            agent_id: input.agent_id,
            effect,
            selector,
            max_risk,
            requires,
            skill_id,
            matches,
            conditions: Vec::new(),
            per_turn: 1,
            per_day: 1,
            sum_field: None,
            sum_per_day: None,
            daily_totals: BTreeMap::new(),
            mode: "on",
            note: word(input.note),
            expires_at,
        });
    }

    // Required, because a condition is per request and bounds nothing on its
    // own: four hundred charges of 4999 each satisfy `amount_pence <= 5000`.
    let per_turn = input
        .per_turn
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("a policy needs a count per turn above zero"))?;
    let per_day = input
        .per_day
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("a policy needs a count per day above zero"))?;
    let sum_field = word(input.sum_field);
    match (&sum_field, input.sum_per_day) {
        (None, None) => {}
        (Some(_), Some(n)) if n >= 0 => {}
        _ => {
            return Err(invalid(
                "a sum per day names one number field and a limit at or above zero",
            ));
        }
    }
    let mode = match input.mode.as_str() {
        "trial" => "trial",
        "on" => "on",
        _ => return Err(invalid("mode is trial or on")),
    };

    Ok(Checked {
        agent_id: input.agent_id,
        effect,
        selector,
        max_risk,
        requires,
        skill_id,
        matches,
        conditions: input.conditions,
        per_turn,
        per_day,
        sum_per_day: input.sum_per_day.filter(|_| sum_field.is_some()),
        sum_field,
        daily_totals: input.daily_totals,
        mode,
        note: word(input.note),
        expires_at,
    })
}

/// The agent and skill a write names have to be ones this workspace can see.
async fn check_references(
    state: &ApiState,
    workspace_id: Uuid,
    checked: &Checked,
) -> Result<(), ApiError> {
    if let Some(agent_id) = checked.agent_id {
        let found: Option<i32> =
            sqlx::query_scalar("select 1 from agents where id = $1 and workspace_id = $2")
                .bind(agent_id)
                .bind(workspace_id)
                .fetch_optional(&state.pool)
                .await
                .map_err(internal)?;
        if found.is_none() {
            return Err(invalid("no such agent in this workspace"));
        }
    }
    if let Some(skill_id) = checked.skill_id {
        // This workspace's own, or the operator's, which a workspace binds
        // without owning.
        let found: Option<i32> =
            sqlx::query_scalar("select 1 from skills where id = $1 and workspace_id in ($2, $3)")
                .bind(skill_id)
                .bind(workspace_id)
                .bind(crate::usage::PLATFORM_WORKSPACE)
                .fetch_optional(&state.pool)
                .await
                .map_err(internal)?;
        if found.is_none() {
            return Err(invalid("no such skill"));
        }
    }
    Ok(())
}

/// `GET /v1/approval-policies`.
pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Page<PolicyView>>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_read(&state, &claims).await?;
    let policies = super::list(&state.pool, claims.workspace_id)
        .await
        .map_err(internal)?;
    Ok(Json(Page {
        items: policies.into_iter().map(view).collect(),
        next: None,
    }))
}

/// `GET /v1/approval-policies/{id}`.
pub async fn get(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<PolicyView>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_read(&state, &claims).await?;
    found(&state, claims.workspace_id, id)
        .await
        .map(|p| Json(view(p)))
}

async fn found(state: &ApiState, workspace_id: Uuid, id: Uuid) -> Result<Policy, ApiError> {
    super::get(&state.pool, workspace_id, id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such policy".to_string()))
}

/// Writes a checked policy as a new row.
async fn insert<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    workspace_id: Uuid,
    id: Uuid,
    subject: Uuid,
    checked: &Checked,
) -> Result<(), ApiError> {
    sqlx::query(
        "insert into approval_policies \
             (workspace_id, id, agent_id, effect, selector, max_risk, requires, skill_id, \
              matches, conditions, per_turn, per_day, sum_field, sum_per_day, mode, note, \
              created_by, updated_by, accepted_by, expires_at, daily_totals) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $17, $17, $18, $19)",
    )
    .bind(workspace_id)
    .bind(id)
    .bind(checked.agent_id)
    .bind(checked.effect)
    .bind(checked.selector)
    .bind(&checked.max_risk)
    .bind(&checked.requires)
    .bind(checked.skill_id)
    .bind(&checked.matches)
    .bind(serde_json::to_value(&checked.conditions).map_err(internal)?)
    .bind(checked.per_turn)
    .bind(checked.per_day)
    .bind(&checked.sum_field)
    .bind(checked.sum_per_day)
    .bind(checked.mode)
    .bind(&checked.note)
    .bind(subject)
    .bind(checked.expires_at)
    .bind(serde_json::to_value(&checked.daily_totals).map_err(internal)?)
    .execute(executor)
    .await
    .map_err(internal)?;

    Ok(())
}

/// `POST /v1/approval-policies`.
pub async fn create(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<WritePolicy>,
) -> Result<(StatusCode, Json<PolicyView>), ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_write(&state, &claims).await?;
    let checked = check(input, Utc::now())?;
    check_references(&state, claims.workspace_id, &checked).await?;

    let id = Uuid::now_v7();
    insert(
        &state.pool,
        claims.workspace_id,
        id,
        claims.subject,
        &checked,
    )
    .await?;

    tracing::info!(
        workspace_id = %claims.workspace_id,
        policy_id = %id,
        set_by = %claims.subject,
        mode = checked.mode,
        "an auto-approval policy was set"
    );
    super::waiting::reconcile(&state, claims.workspace_id).await;
    let policy = found(&state, claims.workspace_id, id).await?;
    Ok((StatusCode::CREATED, Json(view(policy))))
}

/// The one line, as the page sends it: a kind of act, a figure under which its
/// requests in one currency are low, and a daily total -- both in the
/// currency's standard minor units -- for an agent, until a date.
#[derive(Debug, Deserialize)]
pub struct WriteOneLine {
    pub requires: String,
    pub currency: String,
    pub low: i64,
    pub daily_total: i64,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    #[serde(default = "trial")]
    pub mode: String,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// The counts a one-line policy carries. The daily total bounds the money;
/// these only have to be above zero, and the full editor changes them.
const ONE_LINE_PER_TURN: i32 = 1;
const ONE_LINE_PER_DAY: i32 = 20;

/// `POST /v1/approval-policies/one-line`: the low band of every operation of
/// that kind in that currency, where it is not that already, and a policy for
/// the kind up to low with that daily total -- in one transaction, everything
/// checked before anything is written, so a band the floor refuses leaves no
/// policy and a policy refused leaves no band.
///
/// Per operation, never the currency's row, which would move every other
/// operation in the currency too. Each keeps its medium bound where that is
/// still above the new low one.
pub async fn one_line(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<WriteOneLine>,
) -> Result<(StatusCode, Json<PolicyView>), ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_write(&state, &claims).await?;
    let ops = super::bands::operations(&state, claims.workspace_id)
        .await
        .map_err(internal)?;
    let rows = super::bands::of(&state.pool, claims.workspace_id)
        .await
        .map_err(internal)?;
    let writes = super::bands::one_line(&ops, &rows, &input.requires, &input.currency, input.low)?;
    let checked = check(
        WritePolicy {
            agent_id: input.agent_id,
            effect: include(),
            selector: "act".into(),
            max_risk: Some(Risk::Low.as_str().to_string()),
            requires: Some(input.requires.clone()),
            skill_id: None,
            matches: None,
            conditions: Vec::new(),
            per_turn: Some(ONE_LINE_PER_TURN),
            per_day: Some(ONE_LINE_PER_DAY),
            sum_field: None,
            sum_per_day: None,
            daily_totals: BTreeMap::from([(input.currency.clone(), input.daily_total)]),
            mode: input.mode,
            note: None,
            expires_at: input.expires_at,
        },
        Utc::now(),
    )?;
    check_references(&state, claims.workspace_id, &checked).await?;

    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await.map_err(internal)?;
    for write in &writes {
        super::bands::write(&mut tx, claims.workspace_id, claims.subject, write)
            .await
            .map_err(internal)?;
    }
    insert(&mut *tx, claims.workspace_id, id, claims.subject, &checked).await?;
    tx.commit().await.map_err(internal)?;

    tracing::info!(
        workspace_id = %claims.workspace_id,
        policy_id = %id,
        bands_set = writes.len(),
        set_by = %claims.subject,
        mode = checked.mode,
        "a one-line auto-approval policy was set"
    );
    super::waiting::reconcile(&state, claims.workspace_id).await;
    let policy = found(&state, claims.workspace_id, id).await?;
    Ok((StatusCode::CREATED, Json(view(policy))))
}

/// `PUT /v1/approval-policies/{id}`: an edit, recorded as who made it.
///
/// The version moves on, so a turn already running under the old one asks a
/// person for anything it would have let through, rather than carrying on
/// under rules nobody holds any more.
pub async fn update(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<WritePolicy>,
) -> Result<Json<PolicyView>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_write(&state, &claims).await?;
    let checked = check(input, Utc::now())?;
    check_references(&state, claims.workspace_id, &checked).await?;

    let updated = sqlx::query(
        "update approval_policies set \
             agent_id = $3, effect = $4, selector = $5, max_risk = $6, requires = $7, \
             skill_id = $8, matches = $9, conditions = $10, per_turn = $11, per_day = $12, \
             sum_field = $13, sum_per_day = $14, mode = $15, note = $16, expires_at = $17, \
             updated_by = $18, daily_totals = $19, updated_at = now(), version = version + 1 \
         where workspace_id = $1 and id = $2 and revoked_at is null",
    )
    .bind(claims.workspace_id)
    .bind(id)
    .bind(checked.agent_id)
    .bind(checked.effect)
    .bind(checked.selector)
    .bind(&checked.max_risk)
    .bind(&checked.requires)
    .bind(checked.skill_id)
    .bind(&checked.matches)
    .bind(serde_json::to_value(&checked.conditions).map_err(internal)?)
    .bind(checked.per_turn)
    .bind(checked.per_day)
    .bind(&checked.sum_field)
    .bind(checked.sum_per_day)
    .bind(checked.mode)
    .bind(&checked.note)
    .bind(checked.expires_at)
    .bind(claims.subject)
    .bind(serde_json::to_value(&checked.daily_totals).map_err(internal)?)
    .execute(&state.pool)
    .await
    .map_err(internal)?
    .rows_affected();
    if updated == 0 {
        found(&state, claims.workspace_id, id).await?;
        return Err((
            StatusCode::CONFLICT,
            "this policy was revoked; set a new one".to_string(),
        ));
    }
    tracing::info!(
        workspace_id = %claims.workspace_id,
        policy_id = %id,
        set_by = %claims.subject,
        mode = checked.mode,
        "an auto-approval policy was changed"
    );
    super::waiting::reconcile(&state, claims.workspace_id).await;
    Ok(Json(view(found(&state, claims.workspace_id, id).await?)))
}

/// One of the small writes: revoking, expiring now, or accepting the gates
/// published since.
async fn mark(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
    id: Uuid,
    set: &'static str,
    what: &str,
) -> Result<Json<PolicyView>, ApiError> {
    let claims = authenticate(state, headers)?;
    may_write(state, &claims).await?;
    // `set` is one of the three literals below, never anything a caller sent.
    let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update approval_policies set {set} \
         where workspace_id = $1 and id = $2 and revoked_at is null"
    )))
    .bind(claims.workspace_id)
    .bind(id)
    .bind(claims.subject)
    .execute(&state.pool)
    .await
    .map_err(internal)?
    .rows_affected();
    if updated == 0 {
        found(state, claims.workspace_id, id).await?;
        return Err((
            StatusCode::CONFLICT,
            "this policy was already revoked".to_string(),
        ));
    }
    tracing::info!(
        workspace_id = %claims.workspace_id,
        policy_id = %id,
        by = %claims.subject,
        "an auto-approval policy was {what}"
    );
    // Accepting is what settles a policy's waiting operations, and revoking or
    // expiring it leaves nobody to accept them.
    super::waiting::reconcile(state, claims.workspace_id).await;
    Ok(Json(view(found(state, claims.workspace_id, id).await?)))
}

/// `POST /v1/approval-policies/{id}/revoke`. Takes effect on the next request,
/// whoever is running the turn: the gateway's record is the check.
pub async fn revoke(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<PolicyView>, ApiError> {
    mark(
        &state,
        &headers,
        id,
        "revoked_at = now(), revoked_by = $3",
        "revoked",
    )
    .await
}

/// `POST /v1/approval-policies/{id}/expire`: ends it now, leaving it renewable
/// by an edit, where a revoked one is not.
pub async fn expire(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<PolicyView>, ApiError> {
    mark(
        &state,
        &headers,
        id,
        "expires_at = least(expires_at, now()), updated_by = $3, updated_at = now(), \
         version = version + 1",
        "expired",
    )
    .await
}

/// `POST /v1/approval-policies/{id}/accept`: the gates a risk or act selector
/// reached since it was last accepted are covered as the rest are, rather than
/// in trial.
pub async fn accept(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<PolicyView>, ApiError> {
    mark(
        &state,
        &headers,
        id,
        "accepted_at = now(), accepted_by = $3",
        "accepted for the operations published since",
    )
    .await
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    /// Requests it let out, ever and in the last week.
    pub auto_approved: i64,
    pub auto_approved_week: i64,
    /// Questions raised while it was in trial that it would have said yes to,
    /// and what the person said.
    pub trial_approved: i64,
    pub trial_declined: i64,
    pub trial_pending: i64,
    /// The last approvals of what it covers, each with whether its tests hold.
    /// Absent for a risk selector, whose reach depends on declarations this
    /// history does not carry.
    pub history: Option<Vec<Past>>,
}

#[derive(Debug, Serialize)]
pub struct Past {
    pub item_id: Uuid,
    pub at: DateTime<Utc>,
    pub requires: String,
    pub binds: serde_json::Value,
    /// approved, declined or pending.
    pub answer: &'static str,
    /// Whether this policy would have let it out: its tests hold of the bound
    /// values, the risk the amount works out to under the bands as they stand
    /// is within its bound, and the amount fits in the day's total beside what
    /// it would already have let out. Not the fields nobody called harmless,
    /// which a question does not record.
    pub would: bool,
    /// Why not, where the tests held and the amount is what stopped it: its
    /// risk, or the day's total.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One question as the history reads it.
pub struct Asked {
    pub item_id: Uuid,
    pub at: DateTime<Utc>,
    pub payload: serde_json::Value,
    pub state: String,
}

/// What a policy would have made of the questions people were asked, newest
/// first as they are given.
///
/// Decided as the gateway would decide them now: the tests, then the risk the
/// amount works out to under `money`'s bands -- the workspace's, applied --
/// held to the tighter of the policy's bound and `ceiling`, then the day's
/// total, counted over the day before each in the order they were asked and
/// only from what this would have let out. Without the last two a policy for
/// low-risk charges said it would have approved a £290 one.
pub fn replay(
    policy: &Policy,
    requires: &str,
    asked: &[Asked],
    money: &[super::Declared],
    ceiling: Option<Risk>,
) -> Vec<Past> {
    let operation = policy.matches.as_deref().and_then(super::normalize_matches);
    let max = super::bound(policy, ceiling);
    // What it would have let out in each currency, oldest first, for the day's
    // total.
    let mut let_out: Vec<(DateTime<Utc>, String, i64)> = Vec::new();
    let mut past: Vec<Past> = asked
        .iter()
        .rev()
        .filter_map(|a| {
            let payload = &a.payload;
            let method = payload["method"].as_str().unwrap_or_default();
            let host = payload["host"].as_str().unwrap_or_default();
            let path = payload["path"].as_str().unwrap_or_default();
            if let Some(op) = &operation {
                let (want, pattern) = op.split_once(' ')?;
                let normalized = outturn_core::egress::gate::normalize_path(path);
                let pattern = outturn_core::egress::gate::normalize_path(pattern);
                let covers = match pattern.strip_suffix('*') {
                    Some(prefix) => normalized.starts_with(prefix),
                    None => normalized == pattern,
                };
                if method != want || !covers {
                    return None;
                }
            }
            let fields: serde_json::Map<String, serde_json::Value> = payload["binds"]
                .as_array()
                .map(|binds| {
                    binds
                        .iter()
                        .filter_map(|b| {
                            Some((b["field"].as_str()?.to_string(), b["value"].clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let answer = match a.state.as_str() {
                "resolved" => "approved",
                "cancelled" => "declined",
                _ => "pending",
            };
            let mut would = policy::holds(&policy.conditions, &fields);
            let mut why = None;
            let banded = money
                .iter()
                .filter(|d| d.gate.requires == requires)
                .filter(|d| d.gate.covers_request(host, method, path))
                .find_map(|d| Some((d, d.banding.as_ref()?, d.amount.as_ref()?)));
            if would && let Some((_, banding, declared)) = banded {
                let (risk, amount) = banding.assess(&fields);
                let said = |v: i64| {
                    outturn_core::egress::amount::format(
                        v,
                        &banding.currency,
                        declared.amount.minor_units,
                    )
                };
                let total = policy.daily_totals.get(&banding.currency).copied();
                // A day is counted in the currency's standard units, whatever
                // the operation's own.
                let standard =
                    outturn_core::egress::amount::standard_minor_units(&banding.currency);
                let counted =
                    |v: i64| outturn_core::egress::amount::format(v, &banding.currency, standard);
                if risk > max {
                    would = false;
                    why = Some(match amount {
                        Some(v) => format!("{} is {} risk", said(v), risk.as_str()),
                        None => format!("with no amount to read it is {} risk", risk.as_str()),
                    });
                } else if let Some(total) = total {
                    match amount {
                        None => {
                            would = false;
                            why = Some("no amount to count against the day's total".into());
                        }
                        Some(v) => {
                            let v = banding.counted(v).unwrap_or(i64::MAX);
                            let day: i64 = let_out
                                .iter()
                                .filter(|(at, c, _)| {
                                    *c == banding.currency && *at > a.at - chrono::Duration::days(1)
                                })
                                .map(|(_, _, v)| v)
                                .sum();
                            if day.saturating_add(v) > total {
                                would = false;
                                why = Some(format!(
                                    "{} would take the day past {}",
                                    counted(v),
                                    counted(total)
                                ));
                            }
                        }
                    }
                }
                if would && let Some(v) = amount.and_then(|v| banding.counted(v)) {
                    let_out.push((a.at, banding.currency.clone(), v));
                }
            }
            Some(Past {
                item_id: a.item_id,
                at: a.at,
                requires: requires.to_string(),
                binds: payload["binds"].clone(),
                answer,
                would,
                why,
            })
        })
        .collect();
    past.reverse();
    past
}

/// How many of the last requests a history covers.
const HISTORY: i64 = 50;

/// `GET /v1/approval-policies/{id}/evidence`.
pub async fn evidence(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Evidence>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    may_read(&state, &claims).await?;
    let policy = found(&state, claims.workspace_id, id).await?;

    let counts = sqlx::query(
        "select count(*) as ever, \
                count(*) filter (where at > now() - interval '7 days') as week \
         from auto_approvals where workspace_id = $1 and policy_id = $2",
    )
    .bind(claims.workspace_id)
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;

    let trial = sqlx::query(
        "select count(*) filter (where state = 'resolved') as approved, \
                count(*) filter (where state = 'cancelled') as declined, \
                count(*) filter (where state = 'pending') as pending \
         from action_items \
         where workspace_id = $1 and kind like 'approval.%' \
           and payload->>'would_auto_approve' = $2::text",
    )
    .bind(claims.workspace_id)
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;

    let history = match (&policy.selector[..], &policy.requires) {
        ("act" | "operation", Some(requires)) => {
            let rows = sqlx::query(
                "select id, created_at, payload, state from action_items \
                 where workspace_id = $1 and kind = 'approval.' || $2 \
                   and not (payload ? 'followup') \
                 order by id desc limit $3",
            )
            .bind(claims.workspace_id)
            .bind(requires)
            .bind(HISTORY)
            .fetch_all(&state.pool)
            .await
            .map_err(internal)?;
            let asked: Vec<Asked> = rows
                .iter()
                .map(|r| Asked {
                    item_id: r.get("id"),
                    at: r.get("created_at"),
                    payload: r.get("payload"),
                    state: r.get("state"),
                })
                .collect();
            // The bands each money operation runs under now, as a turn prepared
            // now would commit them, so the replay works out risk as the gateway
            // would.
            let mut money = super::bands::operations(&state, claims.workspace_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|(d, _)| d)
                .collect::<Vec<_>>();
            let rows = super::bands::of(&state.pool, claims.workspace_id)
                .await
                .map_err(internal)?;
            super::bands::apply(&mut money, &rows);
            let ceiling = super::ceiling(&state.pool, claims.workspace_id).await;
            let mut past = replay(&policy, requires, &asked, &money, ceiling);
            // A policy that would have approved something a person declined says
            // so first.
            past.sort_by_key(|p| !(p.would && p.answer == "declined"));
            Some(past)
        }
        _ => None,
    };

    Ok(Json(Evidence {
        auto_approved: counts.get("ever"),
        auto_approved_week: counts.get("week"),
        trial_approved: trial.get("approved"),
        trial_declined: trial.get("declined"),
        trial_pending: trial.get("pending"),
        history,
    }))
}

#[derive(Debug, Deserialize)]
pub struct AnsweredQuery {
    pub requires: String,
}

#[derive(Debug, Serialize)]
pub struct Answered {
    pub requires: String,
    /// How many times somebody in this workspace said yes to this act.
    pub approved: i64,
    /// Whether the caller may set a policy, so the prompt offers the link only
    /// to somebody who could follow it.
    pub may_delegate: bool,
}

/// `GET /v1/approvals/answered?requires=charge`: how often this workspace has
/// approved one act, for the prompt deciding whether to offer a policy.
///
/// The caller's own workspace, and only for somebody who answers approvals:
/// the count says how a workspace's people have been deciding, which is theirs.
pub async fn answered(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<AnsweredQuery>,
) -> Result<Json<Answered>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let held = authorities_of(&state, &claims).await?;
    if !held.contains(&Authority::ApprovalsAnswer) {
        return Err(forbidden());
    }
    let approved: i64 = sqlx::query_scalar(
        "select count(*) from action_items \
         where workspace_id = $1 and kind = 'approval.' || $2 and state = 'resolved' \
           and not (payload ? 'followup')",
    )
    .bind(claims.workspace_id)
    .bind(&query.requires)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(Answered {
        requires: query.requires,
        approved,
        may_delegate: claims.family_id.is_some() && held.contains(&Authority::ApprovalsDelegate),
    }))
}

#[derive(Debug, Deserialize)]
pub struct AmountsQuery {
    pub requires: String,
    /// How far back, in days. Thirty unless said; at most a year.
    #[serde(default)]
    pub days: Option<i32>,
}

/// What a workspace approved of one act in one currency.
#[derive(Debug, Serialize)]
pub struct ApprovedIn {
    pub currency: String,
    /// The currency's standard digits after the point, which every figure here
    /// is in.
    pub minor_units: u8,
    /// How many a person said yes to.
    pub approved: i64,
    pub max: super::bands::Money,
    pub total: super::bands::Money,
    /// The figure an offer prefills: the first round amount above the largest
    /// approved -- £50 after approvals all under £40.
    pub suggested_low: super::bands::Money,
    /// The round figure every approved amount was under, which the offer says:
    /// the largest plus one, rounded up at its leading digit -- £40 after a
    /// largest of £39.99.
    pub all_under: super::bands::Money,
}

#[derive(Debug, Serialize)]
pub struct Amounts {
    pub requires: String,
    pub days: i32,
    pub currencies: Vec<ApprovedIn>,
    pub may_delegate: bool,
}

/// The smallest of 1, 2 and 5 times a power of ten, in whole units of the
/// currency, above `max`.
fn round_above(max: i64, minor_units: u8) -> i64 {
    let mut step = 10i64.saturating_pow(u32::from(minor_units));
    loop {
        for m in [1, 2, 5] {
            let n = step.saturating_mul(m);
            if n > max || n == i64::MAX {
                return n;
            }
        }
        step = step.saturating_mul(10);
    }
}

/// One more than `max`, rounded up at its leading digit, and never less than
/// one whole unit of the currency.
fn round_under(max: i64, minor_units: u8) -> i64 {
    let above = max
        .saturating_add(1)
        .max(10i64.saturating_pow(u32::from(minor_units)));
    let magnitude = 10i64.saturating_pow(above.ilog10());
    (above / magnitude + i64::from(above % magnitude != 0)).saturating_mul(magnitude)
}

/// `GET /v1/approvals/amounts?requires=charge&days=30`: the amounts this
/// workspace's people approved of one act, per currency, for the prompt's
/// offer -- "12 charges approved this month, all under £40".
///
/// Scoped as the count beside it is: the caller's workspace, for somebody who
/// answers approvals. Only questions raised since they carried their amount.
pub async fn amounts(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<AmountsQuery>,
) -> Result<Json<Amounts>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    let held = authorities_of(&state, &claims).await?;
    if !held.contains(&Authority::ApprovalsAnswer) {
        return Err(forbidden());
    }
    let days = query.days.unwrap_or(30).clamp(1, 365);
    let rows = sqlx::query(
        "select payload->'money'->>'currency' as currency, \
                (payload->'money'->>'minor_units')::int as minor_units, \
                count(*) as approved, \
                max((payload->'money'->>'amount')::bigint) as max, \
                sum((payload->'money'->>'amount')::bigint)::bigint as total \
         from action_items \
         where workspace_id = $1 and kind = 'approval.' || $2 and state = 'resolved' \
           and not (payload ? 'followup') and payload ? 'money' \
           and created_at > now() - make_interval(days => $3) \
         group by 1, 2 order by 1",
    )
    .bind(claims.workspace_id)
    .bind(&query.requires)
    .bind(days)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    // One figure per currency, in its standard units, whatever each operation
    // counts in: an offer's figure becomes a band for the whole currency.
    let mut per: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    for r in &rows {
        let currency: String = r.get("currency");
        let Ok(minor) = u8::try_from(r.get::<i32, _>("minor_units")) else {
            continue;
        };
        let standard = |v: i64| outturn_core::egress::amount::to_standard(v, &currency, minor);
        let (Some(max), Some(total)) = (standard(r.get("max")), standard(r.get("total"))) else {
            continue;
        };
        let e = per.entry(currency.clone()).or_insert((0, 0, 0));
        e.0 += r.get::<i64, _>("approved");
        e.1 = e.1.max(max);
        e.2 = e.2.saturating_add(total);
    }
    let currencies = per
        .into_iter()
        .map(|(currency, (approved, max, total))| {
            let minor = outturn_core::egress::amount::standard_minor_units(&currency);
            let m = |v| super::bands::Money::of(v, &currency, minor);
            ApprovedIn {
                approved,
                max: m(max),
                total: m(total),
                suggested_low: m(round_above(max, minor)),
                all_under: m(round_under(max, minor)),
                minor_units: minor,
                currency: currency.clone(),
            }
        })
        .collect();
    Ok(Json(Amounts {
        requires: query.requires,
        days,
        currencies,
        may_delegate: claims.family_id.is_some() && held.contains(&Authority::ApprovalsDelegate),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_offer_rounds_up_past_the_largest_approved() {
        assert_eq!(round_above(3999, 2), 5000);
        assert_eq!(round_above(5000, 2), 10000);
        assert_eq!(round_above(12, 2), 100, "never below one whole unit");
        assert_eq!(round_above(1500, 0), 2000);
    }

    #[test]
    fn the_offer_says_a_round_figure_every_amount_was_under() {
        assert_eq!(round_under(3999, 2), 4000);
        assert_eq!(round_under(4000, 2), 5000);
        assert_eq!(round_under(12, 2), 100, "never below one whole unit");
        assert_eq!(round_under(123_456, 2), 200_000);
        assert_eq!(round_under(7, 0), 8);
        assert_eq!(round_under(i64::MAX, 2), i64::MAX);
    }

    fn write(v: serde_json::Value) -> WritePolicy {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_policy_needs_its_caps() {
        let now = Utc::now();
        assert!(check(write(json!({"selector": "act", "requires": "charge"})), now).is_err());
        assert!(
            check(
                write(
                    json!({"selector": "act", "requires": "charge", "per_turn": 0, "per_day": 5})
                ),
                now
            )
            .is_err()
        );
        let ok = check(
            write(json!({"selector": "act", "requires": "charge", "per_turn": 1, "per_day": 5})),
            now,
        )
        .unwrap();
        assert_eq!(ok.mode, "trial", "a policy starts in trial");
    }

    #[test]
    fn a_policy_without_an_expiry_runs_out_in_ninety_days() {
        let now = Utc::now();
        let ok = check(
            write(json!({"selector": "act", "requires": "charge", "per_turn": 1, "per_day": 5})),
            now,
        )
        .unwrap();
        assert_eq!(ok.expires_at, now + chrono::Duration::days(90));

        let too_far = now + chrono::Duration::days(MAX_LIFETIME_DAYS + 1);
        assert!(
            check(
                write(
                    json!({"selector": "act", "requires": "charge", "per_turn": 1,
                             "per_day": 5, "expires_at": too_far})
                ),
                now
            )
            .is_err()
        );
        let past = now - chrono::Duration::seconds(1);
        assert!(
            check(
                write(
                    json!({"selector": "act", "requires": "charge", "per_turn": 1,
                             "per_day": 5, "expires_at": past})
                ),
                now
            )
            .is_err()
        );
    }

    #[test]
    fn a_selector_says_exactly_one_thing() {
        let now = Utc::now();
        for bad in [
            json!({"selector": "risk", "per_turn": 1, "per_day": 1}),
            json!({"selector": "risk", "max_risk": "huge", "per_turn": 1, "per_day": 1}),
            json!({"selector": "operation", "requires": "charge", "per_turn": 1, "per_day": 1}),
            json!({"selector": "operation", "requires": "charge", "skill_id": Uuid::nil(),
                   "matches": "charges", "per_turn": 1, "per_day": 1}),
            json!({"selector": "everything", "per_turn": 1, "per_day": 1}),
        ] {
            assert!(check(write(bad.clone()), now).is_err(), "{bad}");
        }
        let op = check(
            write(
                json!({"selector": "operation", "requires": "charge", "skill_id": Uuid::nil(),
                         "matches": "post /charges", "per_turn": 1, "per_day": 1}),
            ),
            now,
        )
        .unwrap();
        assert_eq!(op.matches.as_deref(), Some("POST /charges"));
    }

    #[test]
    fn an_exclusion_carries_no_tests() {
        let now = Utc::now();
        assert!(
            check(
                write(
                    json!({"selector": "act", "requires": "refund", "effect": "exclude",
                             "conditions": [{"field": "a", "op": "eq", "value": 1}]})
                ),
                now
            )
            .is_err()
        );
        let ok = check(
            write(json!({"selector": "act", "requires": "refund", "effect": "exclude"})),
            now,
        )
        .unwrap();
        assert_eq!(ok.mode, "on");
    }

    #[test]
    fn the_one_line_policy_is_an_act_up_to_a_risk_with_a_daily_total() {
        let now = Utc::now();
        let ok = check(
            write(
                json!({"selector": "act", "requires": "charge", "max_risk": "low",
                         "daily_totals": {"GBP": 50000}, "per_turn": 5, "per_day": 50}),
            ),
            now,
        )
        .unwrap();
        assert_eq!(ok.max_risk.as_deref(), Some("low"));
        assert_eq!(ok.daily_totals.get("GBP"), Some(&50000));
        for bad in [
            json!({"selector": "act", "requires": "charge", "max_risk": "tiny",
                   "per_turn": 1, "per_day": 1}),
            json!({"selector": "act", "requires": "charge", "daily_totals": {"pounds": 5},
                   "per_turn": 1, "per_day": 1}),
            json!({"selector": "act", "requires": "charge", "daily_totals": {"GBP": -5},
                   "per_turn": 1, "per_day": 1}),
            json!({"selector": "act", "requires": "refund", "effect": "exclude",
                   "daily_totals": {"GBP": 5}}),
        ] {
            assert!(check(write(bad.clone()), now).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_language_is_checked_on_the_way_in() {
        let now = Utc::now();
        assert!(
            check(
                write(
                    json!({"selector": "act", "requires": "spam", "per_turn": 1, "per_day": 1,
                             "conditions": [{"field": "folder", "op": "ne", "value": "inbox"}]})
                ),
                now
            )
            .is_err()
        );
    }

    /// A charge as a question records one, `hours` ago.
    fn charge_asked(pence: i64, hours: i64) -> Asked {
        Asked {
            item_id: Uuid::now_v7(),
            at: Utc::now() - chrono::Duration::hours(hours),
            payload: json!({
                "method": "POST", "host": "api.example.com", "path": "/charges",
                "binds": [
                    {"field": "booking_id", "value": "bk_1"},
                    {"field": "amount_pence", "value": pence},
                ],
            }),
            state: "resolved".into(),
        }
    }

    /// The history of a policy for low-risk charges replays what the gateway
    /// would decide now: £290 and £390 are medium and high under the bands, not
    /// approved because no test said otherwise, and the day's total stops what
    /// small ones add up to -- counted in the order they were asked.
    #[test]
    fn the_record_replays_risk_and_the_days_total() {
        use outturn_core::egress::amount::{Amount, Banding, Bands, Declaration, Floor};
        let mut d = super::super::tests::declared("charge", Risk::High);
        let bands = Bands {
            low: Some(5000),
            medium: Some(30000),
        };
        d.amount = Some(Declaration {
            amount: Amount {
                field: "amount_pence".into(),
                currency: "GBP".into(),
                minor_units: 2,
                currency_field: None,
            },
            bands: bands.clone(),
            floor: Floor::default(),
        });
        d.banding = Some(Banding {
            field: "amount_pence".into(),
            currency: "GBP".into(),
            currency_field: None,
            minor_units: 2,
            base: Risk::High,
            bands,
            floor: Floor::default(),
        });
        let mut low = super::super::tests::policy("act");
        low.conditions = Vec::new();
        low.max_risk = Some("low".into());
        low.daily_totals = BTreeMap::from([("GBP".to_string(), 10000)]);

        // Newest first, as the page reads them.
        let asked = vec![
            charge_asked(39000, 1),
            charge_asked(29000, 2),
            charge_asked(4000, 3),
            charge_asked(4000, 4),
            charge_asked(4000, 5),
        ];
        let past = replay(&low, "charge", &asked, &[d.clone()], Some(Risk::Medium));
        let said: Vec<(bool, Option<&str>)> =
            past.iter().map(|p| (p.would, p.why.as_deref())).collect();
        assert_eq!(
            said,
            vec![
                (false, Some("£390.00 is high risk")),
                (false, Some("£290.00 is medium risk")),
                (false, Some("£40.00 would take the day past £100.00")),
                (true, None),
                (true, None),
            ]
        );
        assert_eq!(past[0].item_id, asked[0].item_id, "still newest first");

        // A day later the total starts again.
        let spread = vec![
            charge_asked(4000, 1),
            charge_asked(4000, 26),
            charge_asked(4000, 27),
        ];
        let past = replay(&low, "charge", &spread, &[d.clone()], Some(Risk::Medium));
        assert!(past.iter().all(|p| p.would), "{past:?}");

        // The ceiling is a bound too: a medium policy under a low cap is low.
        low.max_risk = Some("medium".into());
        low.daily_totals = BTreeMap::new();
        let past = replay(&low, "charge", &asked[..2], &[d.clone()], Some(Risk::Low));
        assert!(past.iter().all(|p| !p.would), "{past:?}");
        let past = replay(&low, "charge", &asked[..2], &[d], Some(Risk::Medium));
        assert_eq!(
            past.iter().map(|p| p.would).collect::<Vec<_>>(),
            vec![false, true]
        );

        // An operation that declares no amount is decided by the tests alone,
        // as before.
        let plain = super::super::tests::declared("charge", Risk::Low);
        let past = replay(&low, "charge", &asked[..1], &[plain], Some(Risk::Medium));
        assert!(past[0].would && past[0].why.is_none());
    }
}
