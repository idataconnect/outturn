//! Turns that start because the clock said so.
//!
//! A schedule is a row, a cron expression and a timezone. Firing one means
//! creating a session, storing the prompt as a message nobody sent, and
//! enqueueing a turn at background priority -- after which the schedule writes
//! its own successor.
//!
//! Nothing here is on the turn path. The loop wakes, takes what is due, and
//! goes back to sleep.

pub mod cron;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use cron::Cron;

/// How often the firing loop looks for work.
///
/// Cron's resolution is a minute, so checking more often finds nothing and
/// checking much less often means a schedule fires late by however long the
/// gap is. Thirty seconds keeps the lateness under a minute without asking a
/// question that is almost always answered "nothing".
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub name: String,
    pub prompt: String,
    pub expression: String,
    pub timezone: String,
    pub enabled: bool,
    pub account: Option<String>,
    pub owner_id: Option<Uuid>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub skipped: i32,
    pub created_at: DateTime<Utc>,
}

/// What a caller may set.
#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleInput {
    pub agent_id: Uuid,
    pub name: String,
    pub prompt: String,
    pub expression: String,
    #[serde(default = "utc")]
    pub timezone: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The workspace's label for whose work this is, for the usage ledger.
    #[serde(default)]
    pub account: Option<String>,
}

fn utc() -> String {
    "UTC".to_string()
}

fn yes() -> bool {
    true
}

/// Everything wrong with a proposed schedule, refused before it is stored.
///
/// Checked here rather than at the firing loop because a schedule that cannot
/// be parsed will never fire, and finding that out at 9am tomorrow is finding
/// it out from its absence.
pub fn validate(input: &ScheduleInput) -> Result<(Cron, Tz), String> {
    if input.name.trim().is_empty() {
        return Err("a schedule needs a name".to_string());
    }
    if input.prompt.trim().is_empty() {
        return Err("a schedule needs something to say".to_string());
    }
    let cron = Cron::parse(&input.expression)?;
    let tz: Tz = input
        .timezone
        .parse()
        .map_err(|_| format!("'{}' is not an IANA timezone name", input.timezone))?;
    Ok((cron, tz))
}

/// The next few firings, for somebody deciding whether they wrote what they
/// meant.
///
/// The single most useful thing the editor shows: an expression is exact and
/// unreadable, and dates are the only form in which a mistake is obvious
/// before it has cost a day.
pub fn upcoming(cron: &Cron, tz: Tz, from: DateTime<Utc>, count: usize) -> Vec<DateTime<Utc>> {
    let mut out = Vec::with_capacity(count);
    let mut at = from;
    for _ in 0..count {
        match cron.next_after(at, tz) {
            Some(next) => {
                out.push(next);
                at = next;
            }
            None => break,
        }
    }
    out
}

/// Where the next firing goes, given where the last one was owed.
///
/// A deployment down overnight should not wake to twenty-four queued turns, so
/// anything already past is passed over rather than run late. The count of
/// what was skipped is returned and stored, because a schedule that quietly
/// missed a week looks exactly like one that never worked.
pub fn advance(
    cron: &Cron,
    tz: Tz,
    owed: DateTime<Utc>,
    now: DateTime<Utc>,
) -> (Option<DateTime<Utc>>, i32) {
    let mut at = owed;
    let mut skipped = 0;
    // Bounded so a pathological expression cannot hold the loop: at a firing a
    // minute, this is a day's worth of catching up before giving up on the
    // rest.
    for _ in 0..1440 {
        match cron.next_after(at, tz) {
            Some(next) if next <= now => {
                skipped += 1;
                at = next;
            }
            other => return (other, skipped),
        }
    }
    (cron.next_after(at, tz), skipped)
}

pub mod postgres;
pub mod worker;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_missed_night_does_not_queue_a_turn_for_every_hour_of_it() {
        // Hourly, owed since midnight, and nothing ran until nine. The point
        // of the skip is that nine turns do not arrive at once -- somebody
        // whose deployment was down overnight wants this morning's report, not
        // nine of yesterday's.
        let cron = Cron::parse("0 * * * *").expect("parse");
        let (next, skipped) = advance(
            &cron,
            Tz::UTC,
            at("2026-09-20T00:00:00Z"),
            at("2026-09-20T09:30:00Z"),
        );
        // Nine, not ten: the search runs strictly after the firing that was
        // owed, so midnight's own is the one that just happened rather than
        // one of the ones that were missed.
        assert_eq!(skipped, 9, "01:00 through 09:00");
        assert_eq!(
            next.expect("a next firing").to_rfc3339(),
            "2026-09-20T10:00:00+00:00"
        );
    }

    #[test]
    fn a_schedule_that_is_up_to_date_skips_nothing() {
        let cron = Cron::parse("0 9 * * *").expect("parse");
        let now = at("2026-09-20T09:00:00Z");
        let (next, skipped) = advance(&cron, Tz::UTC, now, now);
        assert_eq!(skipped, 0);
        assert_eq!(
            next.expect("next").to_rfc3339(),
            "2026-09-21T09:00:00+00:00"
        );
    }

    #[test]
    fn upcoming_walks_forward_rather_than_repeating_itself() {
        // Each entry has to be strictly after the last, or the editor shows
        // the same date three times and nobody can check a weekly pattern.
        let cron = Cron::parse("0 9 * * 1-5").expect("parse");
        let next = upcoming(&cron, Tz::UTC, at("2026-09-18T18:00:00Z"), 3);
        let shown: Vec<String> = next.iter().map(|d| d.to_rfc3339()).collect();
        assert_eq!(
            shown,
            vec![
                "2026-09-21T09:00:00+00:00",
                "2026-09-22T09:00:00+00:00",
                "2026-09-23T09:00:00+00:00",
            ]
        );
    }

    #[test]
    fn an_expression_that_never_matches_yields_no_firings_rather_than_hanging() {
        let cron = Cron::parse("0 0 30 2 *").expect("parses");
        assert!(upcoming(&cron, Tz::UTC, at("2026-09-20T00:00:00Z"), 3).is_empty());
    }

    #[test]
    fn validation_refuses_what_would_never_fire() {
        let bad = ScheduleInput {
            agent_id: uuid::Uuid::nil(),
            name: "x".into(),
            prompt: "y".into(),
            expression: "nonsense".into(),
            timezone: "UTC".into(),
            enabled: true,
            account: None,
        };
        assert!(validate(&bad).is_err());

        let bad_zone = ScheduleInput {
            expression: "0 9 * * *".into(),
            timezone: "Mars/Olympus".into(),
            ..bad.clone()
        };
        assert!(validate(&bad_zone).unwrap_err().contains("IANA"));
    }
}
