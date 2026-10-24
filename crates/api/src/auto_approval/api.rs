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
}

fn view(policy: Policy) -> PolicyView {
    let state = policy.state(Utc::now());
    PolicyView { policy, state }
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
    sqlx::query(
        "insert into approval_policies \
             (workspace_id, id, agent_id, effect, selector, max_risk, requires, skill_id, \
              matches, conditions, per_turn, per_day, sum_field, sum_per_day, mode, note, \
              created_by, updated_by, accepted_by, expires_at, daily_totals) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $17, $17, $18, $19)",
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
    .bind(claims.subject)
    .bind(checked.expires_at)
    .bind(serde_json::to_value(&checked.daily_totals).map_err(internal)?)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

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
    /// Whether this policy's tests hold of the bound values. Only the tests: a
    /// field nobody called harmless is not in what a question records.
    pub would: bool,
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
            let operation = policy.matches.as_deref().and_then(super::normalize_matches);
            let mut past: Vec<Past> = rows
                .iter()
                .filter_map(|r| {
                    let payload: serde_json::Value = r.get("payload");
                    if let Some(op) = &operation {
                        let (method, pattern) = op.split_once(' ')?;
                        let path = payload["path"].as_str()?;
                        let normalized = outturn_core::egress::gate::normalize_path(path);
                        let pattern = outturn_core::egress::gate::normalize_path(pattern);
                        let covers = match pattern.strip_suffix('*') {
                            Some(prefix) => normalized.starts_with(prefix),
                            None => normalized == pattern,
                        };
                        if payload["method"].as_str() != Some(method) || !covers {
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
                    let answer = match r.get::<String, _>("state").as_str() {
                        "resolved" => "approved",
                        "cancelled" => "declined",
                        _ => "pending",
                    };
                    Some(Past {
                        item_id: r.get("id"),
                        at: r.get("created_at"),
                        requires: requires.clone(),
                        would: policy::holds(&policy.conditions, &fields),
                        binds: payload["binds"].clone(),
                        answer,
                    })
                })
                .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
