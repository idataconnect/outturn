//! `/v1/schedules` — reading and writing what an agent does on its own.
//!
//! Guarded by the agent authorities rather than authorities of its own. A
//! schedule is part of how an agent is configured: whoever may change what an
//! agent does may decide when it does it, and a separate permission would
//! divide one decision across two roles.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::router::{ApiError, ApiState, authorize};
use super::schedule::{self, Schedule, ScheduleInput, postgres};
use crate::auth::rbac::Authority;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    agent_id: Option<Uuid>,
}

/// A schedule plus what it will do next, which is not stored.
///
/// Computed on read rather than kept, because the answer changes with the
/// clock and a stored one would be stale the moment it was written. It is the
/// single most useful thing the editor shows: an expression is exact and
/// unreadable, and dates are the only form in which a mistake is obvious
/// before it has cost a day.
#[derive(Debug, Serialize)]
pub struct WithUpcoming {
    #[serde(flatten)]
    schedule: Schedule,
    upcoming: Vec<DateTime<Utc>>,
    /// Why this schedule will never fire, when that is the case. A row that
    /// says nothing about itself is how a broken expression goes unnoticed.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
}

/// How many firings to show. Three is enough to see a weekly pattern and short
/// enough to read at a glance.
const UPCOMING: usize = 3;

fn decorate(schedule: Schedule) -> WithUpcoming {
    let parsed = schedule::Cron::parse(&schedule.expression);
    let zone = schedule.timezone.parse();

    match (parsed, zone) {
        (Ok(cron), Ok(tz)) => {
            let upcoming = schedule::upcoming(&cron, tz, Utc::now(), UPCOMING);
            let problem = if upcoming.is_empty() {
                Some("this expression never matches a real date".to_string())
            } else {
                None
            };
            WithUpcoming {
                schedule,
                upcoming,
                problem,
            }
        }
        (Err(e), _) => WithUpcoming {
            schedule,
            upcoming: Vec::new(),
            problem: Some(e),
        },
        (_, Err(_)) => {
            let problem = format!("'{}' is not an IANA timezone name", schedule.timezone);
            WithUpcoming {
                schedule,
                upcoming: Vec::new(),
                problem: Some(problem),
            }
        }
    }
}

pub async fn list_schedules(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<WithUpcoming>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let rows = postgres::list(&state.pool, claims.workspace_id, q.agent_id)
        .await
        .map_err(internal)?;
    Ok(Json(rows.into_iter().map(decorate).collect()))
}

pub async fn get_schedule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<WithUpcoming>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let found = postgres::get(&state.pool, claims.workspace_id, id)
        .await
        .map_err(internal)?;
    match found {
        Some(s) => Ok(Json(decorate(s))),
        None => Err((StatusCode::NOT_FOUND, "no such schedule".to_string())),
    }
}

pub async fn create_schedule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<ScheduleInput>,
) -> Result<(StatusCode, Json<WithUpcoming>), ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;

    // Refused here rather than at the firing loop. A schedule that cannot be
    // parsed will never fire, and finding that out tomorrow morning means
    // finding it out from its absence.
    let (cron, tz) = schedule::validate(&input).map_err(bad_request)?;
    let next = cron.next_after(Utc::now(), tz);

    // The owner is the person creating it, which is a different question from
    // who is waiting for the reply -- nobody is. See docs/triggers.md.
    let owner = Some(claims.subject);

    let created = postgres::create(&state.pool, claims.workspace_id, owner, &input, next)
        .await
        .map_err(internal)?;

    tracing::info!(
        actor = %claims.subject,
        schedule_id = %created.id,
        agent_id = %created.agent_id,
        "schedule created"
    );
    Ok((StatusCode::CREATED, Json(decorate(created))))
}

pub async fn update_schedule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<ScheduleInput>,
) -> Result<Json<WithUpcoming>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    let (cron, tz) = schedule::validate(&input).map_err(bad_request)?;

    // Recomputed rather than kept. An edit to the expression or the zone makes
    // the stored firing wrong, and leaving it would run the old schedule once
    // more before the new one took effect -- at a time the person editing it
    // has just decided they did not want.
    let next = if input.enabled {
        cron.next_after(Utc::now(), tz)
    } else {
        // A disabled schedule points nowhere, so re-enabling it computes a
        // fresh time rather than firing immediately for one it slept through.
        None
    };

    let updated = postgres::update(&state.pool, claims.workspace_id, id, &input, next)
        .await
        .map_err(internal)?;

    match updated {
        Some(s) => {
            tracing::info!(actor = %claims.subject, schedule_id = %s.id, "schedule updated");
            Ok(Json(decorate(s)))
        }
        None => Err((StatusCode::NOT_FOUND, "no such schedule".to_string())),
    }
}

pub async fn delete_schedule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    let gone = postgres::delete(&state.pool, claims.workspace_id, id)
        .await
        .map_err(internal)?;
    if gone {
        tracing::info!(actor = %claims.subject, schedule_id = %id, "schedule deleted");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "no such schedule".to_string()))
    }
}

/// What an expression means and when it would next fire, without saving it.
///
/// So the editor can show the next firings while somebody is still typing,
/// which is what catches a wrong expression before it costs a day rather than
/// after.
#[derive(Debug, Deserialize)]
pub struct PreviewInput {
    expression: String,
    #[serde(default)]
    timezone: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Preview {
    upcoming: Vec<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
}

pub async fn preview_schedule(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<PreviewInput>,
) -> Result<Json<Preview>, ApiError> {
    authorize(&state, &headers, Authority::AgentsRead).await?;

    let tz_name = input.timezone.unwrap_or_else(|| "UTC".to_string());
    let cron = match schedule::Cron::parse(&input.expression) {
        Ok(c) => c,
        Err(problem) => {
            return Ok(Json(Preview {
                upcoming: Vec::new(),
                problem: Some(problem),
            }));
        }
    };
    let tz = match tz_name.parse() {
        Ok(tz) => tz,
        Err(_) => {
            return Ok(Json(Preview {
                upcoming: Vec::new(),
                problem: Some(format!("'{tz_name}' is not an IANA timezone name")),
            }));
        }
    };

    // Five here rather than the three a row shows: somebody watching this
    // while typing is checking a pattern, and two more costs nothing.
    let upcoming = schedule::upcoming(&cron, tz, Utc::now(), 5);
    let problem = if upcoming.is_empty() {
        Some("this expression never matches a real date".to_string())
    } else {
        None
    };
    Ok(Json(Preview { upcoming, problem }))
}

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn bad_request(e: String) -> ApiError {
    (StatusCode::BAD_REQUEST, e)
}
