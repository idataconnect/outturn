//! Five-field cron, parsed and stepped in a named timezone.
//!
//! Written here rather than taken as a dependency because the parsing is small
//! and the part that matters is not the parsing: it is that "every weekday at
//! 9" means nine where somebody is, across the two days a year when that is
//! not a fixed offset from UTC. `chrono-tz` already knows those rules, so what
//! is left is deciding which local times to ask it about.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// A parsed five-field expression: minute, hour, day-of-month, month,
/// day-of-week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Vec<u32>,
    months: Vec<u32>,
    weekdays: Vec<u32>,
    /// Whether day-of-month and day-of-week were both narrowed.
    ///
    /// Cron's oldest wart: when both are restricted the match is their *union*
    /// rather than their intersection, so `0 0 1 * 1` is the first of the
    /// month and every Monday, not Mondays that fall on the first. Kept
    /// because every other implementation does it and a schedule copied from
    /// somewhere else must not mean something different here.
    both_days: bool,
}

/// How far ahead a search will look before giving up.
///
/// Four years, so a 29 February expression finds its leap year. An expression
/// that matches nothing -- 31 February -- terminates here rather than looping.
const HORIZON_DAYS: i64 = 366 * 4;

/// How far back the first day's scan reaches before the instant check takes
/// over. One hour, which is the largest offset change any zone applies.
const MINUTES_PER_HOUR: u32 = 60;

impl Cron {
    pub fn parse(expression: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "expected 5 fields (minute hour day month weekday), got {}",
                fields.len()
            ));
        }

        let minutes = field(fields[0], 0, 59, "minute")?;
        let hours = field(fields[1], 0, 23, "hour")?;
        let days = field(fields[2], 1, 31, "day of month")?;
        let months = field(fields[3], 1, 12, "month")?;
        let weekdays = field(fields[4], 0, 6, "day of week")?;

        // Whether each day field actually narrows anything, judged by what it
        // expanded to rather than by how it was written. `*/1` and `0-6` both
        // restrict nothing while differing from the text "*", and comparing
        // the text made `0 0 1 * */1` take the union branch and fire daily
        // instead of on the first of the month.
        let restricts_dom = days.len() < 31;
        let restricts_dow = weekdays.len() < 7;

        Ok(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            both_days: restricts_dom && restricts_dow,
        })
    }

    /// The first firing strictly after `after`.
    ///
    /// Strictly, so a schedule that has just fired computes its successor
    /// rather than itself, which is what keeps the firing loop from spinning
    /// on one row.
    pub fn next_after(&self, after: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
        let local = after.with_timezone(&tz);
        // Seconds are not a field, so every candidate is on a minute boundary
        // and the one containing `after` has already been and gone.
        let mut date = local.date_naive();
        let start_minute = local.hour() * 60 + local.minute() + 1;

        for day_offset in 0..HORIZON_DAYS {
            if day_offset > 0 {
                date = date.succ_opt()?;
            }
            if !self.matches_date(date) {
                continue;
            }
            // On the morning an hour repeats, a wall-clock time inside it maps
            // to two instants. Whether the second is a firing depends on what
            // the expression means rather than on the calendar:
            //
            // - An hourly or sub-hourly schedule owes work in both halves, so
            //   the repeated hour is a real firing and skipping it loses an
            //   hour of work once a year.
            // - A daily schedule owes one firing that day. Its wall-clock time
            //   simply happens twice, and running it twice would act on the
            //   world twice.
            //
            // So the scan reaches back into the repeated hour only when the
            // expression fires more than once an hour. Otherwise the ordinary
            // minute-of-day filter applies and the second occurrence is never
            // considered.
            let reach_back = if self.fires_within_an_hour() {
                MINUTES_PER_HOUR
            } else {
                0
            };
            let from = if day_offset == 0 {
                start_minute.saturating_sub(reach_back)
            } else {
                0
            };
            for &hour in &self.hours {
                for &minute in &self.minutes {
                    if hour * 60 + minute < from {
                        continue;
                    }
                    // Every instant this wall-clock time maps to, earliest
                    // first. Usually one; two on the autumn morning the clocks
                    // go back. The first genuinely ahead of `after` is the
                    // answer.
                    for at in resolve(date, hour, minute, tz) {
                        if at > after {
                            return Some(at);
                        }
                    }
                }
            }
        }
        None
    }

    /// Whether this expression fires more than once in an hour.
    ///
    /// Which is the same question as "is a repeated wall-clock hour two
    /// firings or one". An expression naming every hour, or several minutes
    /// within an hour, owes work in both halves of the hour the clocks give
    /// back; one naming a single time of day owes one firing whose wall-clock
    /// time merely happens twice.
    fn fires_within_an_hour(&self) -> bool {
        self.hours.len() == 24 || self.minutes.len() > 1
    }

    fn matches_date(&self, date: NaiveDate) -> bool {
        if !self.months.contains(&date.month()) {
            return false;
        }
        let dom = self.days.contains(&date.day());
        let dow = self
            .weekdays
            .contains(&(date.weekday().num_days_from_sunday()));
        if self.both_days {
            dom || dow
        } else {
            dom && dow
        }
    }
}

/// Every instant a local wall-clock time maps to, earliest first.
///
/// Usually one. Two on the autumn morning the clocks go back, and none on the
/// spring morning they go forward -- which is why this returns a list rather
/// than an answer.
///
/// **Spring forward**, and the time does not exist: 02:30 is skipped entirely,
/// so a schedule set for it would silently not run that day. It fires at the
/// next time that does exist instead, which is what somebody who asked for
/// "half past two, daily" meant by it.
///
/// **Autumn back**, and the time happens twice. Both are returned, and the
/// caller takes the first that is ahead of where it is searching from. That
/// makes a daily schedule fire once -- the second occurrence is behind the
/// next day's search -- while an hourly one fires in both, which is what an
/// hourly schedule means. Returning only the earlier made the repeated hour
/// unreachable: it always sits behind the firing that preceded it, so an
/// hourly schedule skipped from 01:00 straight to 02:00 and lost an hour of
/// work once a year with nothing recorded.
fn resolve(date: NaiveDate, hour: u32, minute: u32, tz: Tz) -> Vec<DateTime<Utc>> {
    let Some(naive) = date.and_hms_opt(hour, minute, 0) else {
        return Vec::new();
    };
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(at) => vec![at.with_timezone(&Utc)],
        chrono::LocalResult::Ambiguous(earlier, later) => {
            vec![earlier.with_timezone(&Utc), later.with_timezone(&Utc)]
        }
        chrono::LocalResult::None => {
            // Walk forward a minute at a time to the far side of the gap. A
            // gap is an hour at most, so this is bounded and short.
            for extra in 1..=120 {
                let shifted = naive + chrono::Duration::minutes(extra);
                if let chrono::LocalResult::Single(at) = tz.from_local_datetime(&shifted) {
                    return vec![at.with_timezone(&Utc)];
                }
            }
            Vec::new()
        }
    }
}

/// One field: `*`, a number, a list, a range, or any of those with a step.
fn field(spec: &str, min: u32, max: u32, name: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("{name}: '{step}' is not a step"))?;
                if step == 0 {
                    return Err(format!("{name}: step cannot be zero"));
                }
                // A step wider than the field collapses to a single value, so
                // `*/70` silently becomes "on the hour" rather than anything
                // resembling every seventy minutes. Every other bad input here
                // is refused with a message; this one used to pass and be
                // reinterpreted, which the preview then confirmed as correct.
                if step > max - min {
                    return Err(format!(
                        "{name}: a step of {step} is wider than {min}-{max}, so it would mean a single value"
                    ));
                }
                (range, step)
            }
            None => (part, 1),
        };

        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((lo, hi)) = range.split_once('-') {
            (bound(lo, min, max, name)?, bound(hi, min, max, name)?)
        } else {
            let at = bound(range, min, max, name)?;
            // A bare number with a step means "from here onwards", which is
            // what `*/5` and `7/5` both rely on.
            if step > 1 { (at, max) } else { (at, at) }
        };

        if lo > hi {
            return Err(format!("{name}: {lo}-{hi} counts backwards"));
        }
        out.extend((lo..=hi).step_by(step as usize));
    }

    if out.is_empty() {
        return Err(format!("{name}: matches nothing"));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn bound(value: &str, min: u32, max: u32, name: &str) -> Result<u32, String> {
    let n: u32 = value
        .trim()
        .parse()
        .map_err(|_| format!("{name}: '{value}' is not a number"))?;
    if n < min || n > max {
        return Err(format!("{name}: {n} is outside {min}-{max}"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn next(expr: &str, from: &str, tz: &str) -> String {
        Cron::parse(expr)
            .expect("parse")
            .next_after(at(from), tz.parse().expect("tz"))
            .expect("a next firing")
            .to_rfc3339()
    }

    #[test]
    fn a_daily_time_is_the_same_wall_clock_either_side_of_a_dst_change() {
        // Los Angeles springs forward on 2026-03-08. Nine in the morning is
        // 17:00Z before and 16:00Z after, and a schedule that meant "nine"
        // must keep meaning it -- an offset stored instead of a zone is what
        // gets this wrong.
        assert_eq!(
            next("0 9 * * *", "2026-03-07T18:00:00Z", "America/Los_Angeles"),
            "2026-03-08T16:00:00+00:00"
        );
        assert_eq!(
            next("0 9 * * *", "2026-03-06T18:00:00Z", "America/Los_Angeles"),
            "2026-03-07T17:00:00+00:00"
        );
    }

    #[test]
    fn a_time_that_does_not_exist_fires_at_the_next_one_that_does() {
        // 02:30 does not happen on the morning the clocks go forward. A
        // schedule set for it should not silently skip the day.
        let fired = next("30 2 * * *", "2026-03-08T00:00:00Z", "America/Los_Angeles");
        assert_eq!(fired, "2026-03-08T10:00:00+00:00");
    }

    #[test]
    fn a_daily_schedule_fires_once_on_the_day_an_hour_repeats() {
        // Los Angeles falls back on 2026-11-01: 01:30 occurs at 08:30Z and
        // again at 09:30Z. A daily schedule takes the first and then searches
        // from there, so the second occurrence is behind the next day's search
        // and never fires -- once, which is what daily means.
        assert_eq!(
            next("30 1 * * *", "2026-11-01T00:00:00Z", "America/Los_Angeles"),
            "2026-11-01T08:30:00+00:00"
        );
        assert_eq!(
            next("30 1 * * *", "2026-11-01T08:30:00Z", "America/Los_Angeles"),
            "2026-11-02T09:30:00+00:00",
            "the next firing is the following day, not the repeated hour"
        );
    }

    #[test]
    fn an_hourly_schedule_fires_in_both_halves_of_a_repeated_hour() {
        // The same morning, hourly. 01:00 happens at 08:00Z and again at
        // 09:00Z, and both are firings an hourly schedule owes -- taking only
        // the earlier made 09:00Z unreachable and lost an hour of work once a
        // year, silently.
        let c = Cron::parse("0 * * * *").expect("parse");
        let tz: Tz = "America/Los_Angeles".parse().expect("tz");
        let mut at = at("2026-11-01T06:30:00Z");
        let mut seen = Vec::new();
        for _ in 0..5 {
            at = c.next_after(at, tz).expect("next");
            seen.push(at.to_rfc3339());
        }
        assert_eq!(
            seen,
            vec![
                "2026-11-01T07:00:00+00:00",
                "2026-11-01T08:00:00+00:00",
                "2026-11-01T09:00:00+00:00",
                "2026-11-01T10:00:00+00:00",
                "2026-11-01T11:00:00+00:00",
            ]
        );
    }

    #[test]
    fn a_day_field_that_restricts_nothing_does_not_trigger_the_union() {
        // `*/1` expands to every weekday, so it restricts nothing -- but it is
        // not the text "*". Judged by the text, this took cron's union branch
        // and fired daily instead of on the first of the month.
        assert_eq!(
            next("0 0 1 * */1", "2026-09-20T01:00:00Z", "UTC"),
            "2026-10-01T00:00:00+00:00"
        );
        // And `0-6` in the same position, written out.
        assert_eq!(
            next("0 0 1 * 0-6", "2026-09-20T01:00:00Z", "UTC"),
            "2026-10-01T00:00:00+00:00"
        );
    }

    #[test]
    fn a_step_wider_than_its_field_is_refused_rather_than_reinterpreted() {
        // `*/70` used to parse as minutes [0] -- hourly -- and the preview
        // confirmed it as correct. Every other bad input here is refused.
        let e = Cron::parse("*/70 * * * *").expect_err("accepted");
        assert!(e.contains("wider than"), "{e}");
        assert!(Cron::parse("0 */40 * * *").is_err());
        // A step that fits is still fine.
        assert!(Cron::parse("*/30 * * * *").is_ok());
    }

    #[test]
    fn weekdays_skip_the_weekend() {
        // Friday 18:00Z is after Friday's firing, so the next is Monday.
        assert_eq!(
            next("0 9 * * 1-5", "2026-09-18T18:00:00Z", "UTC"),
            "2026-09-21T09:00:00+00:00"
        );
    }

    #[test]
    fn the_next_firing_is_strictly_after_the_moment_asked_about() {
        // A schedule computing its successor at the instant it fired must not
        // get itself back, or the firing loop spins on one row.
        assert_eq!(
            next("0 9 * * *", "2026-09-20T09:00:00Z", "UTC"),
            "2026-09-21T09:00:00+00:00"
        );
    }

    #[test]
    fn day_of_month_and_day_of_week_are_a_union_when_both_are_given() {
        // Cron's oldest wart, kept deliberately: the first of the month OR a
        // Monday, not Mondays falling on the first. 2026-10-01 is a Thursday.
        let c = Cron::parse("0 0 1 * 1").expect("parse");
        let from = at("2026-09-29T00:00:00Z");
        let first = c.next_after(from, Tz::UTC).expect("next");
        assert_eq!(first.to_rfc3339(), "2026-10-01T00:00:00+00:00");
    }

    #[test]
    fn steps_and_lists_parse() {
        assert_eq!(
            next("*/15 * * * *", "2026-09-20T10:02:00Z", "UTC"),
            "2026-09-20T10:15:00+00:00"
        );
        assert_eq!(
            next("0 9,17 * * *", "2026-09-20T10:00:00Z", "UTC"),
            "2026-09-20T17:00:00+00:00"
        );
    }

    #[test]
    fn an_expression_that_can_never_match_is_refused_rather_than_searched_forever() {
        let c = Cron::parse("0 0 31 2 *").expect("parses; it is the date that cannot happen");
        assert!(c.next_after(at("2026-09-20T00:00:00Z"), Tz::UTC).is_none());
    }

    #[test]
    fn bad_expressions_say_what_is_wrong() {
        assert!(Cron::parse("0 9 * *").unwrap_err().contains("5 fields"));
        assert!(Cron::parse("0 99 * * *").unwrap_err().contains("outside"));
        assert!(
            Cron::parse("0 9 * * x")
                .unwrap_err()
                .contains("not a number")
        );
        assert!(
            Cron::parse("0 9-5 * * *")
                .unwrap_err()
                .contains("backwards")
        );
        assert!(Cron::parse("*/0 * * * *").unwrap_err().contains("zero"));
    }
}
