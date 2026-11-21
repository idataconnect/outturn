//! An instant, as every tier stores, sends and compares it.
//!
//! The arithmetic and the time zones are jiff's. Postgres is reached through
//! sqlx's `time` support, since sqlx decodes into no other maintained crate,
//! so `time` appears here and nowhere else.
//!
//! What a `Timestamp` looks like as text is fixed by what has already been
//! written: JSON sent to clients and stored in the event log, idempotency keys,
//! headers. It is RFC 3339 in UTC with the fraction in whole milli-, micro- or
//! nanoseconds -- `2026-09-18T18:00:00.123400Z`, never `.1234` -- and a change
//! to it is a change to a wire format.

use std::fmt::Write as _;
use std::ops::{Add, AddAssign, Sub, SubAssign};

pub use jiff::SignedDuration;

/// An instant in UTC, to the nanosecond.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(jiff::Timestamp);

/// How many digits of a second [`Timestamp::to_rfc3339_opts`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondsFormat {
    /// Whole seconds, the fraction dropped.
    Secs,
    /// None when the fraction is zero, else three, six or nine digits:
    /// the fewest that hold it exactly.
    AutoSi,
}

impl Timestamp {
    pub const UNIX_EPOCH: Timestamp = Timestamp(jiff::Timestamp::UNIX_EPOCH);

    pub fn now() -> Self {
        Self(jiff::Timestamp::now())
    }

    /// Seconds and nanoseconds since the epoch; `None` out of range.
    pub fn from_unix(secs: i64, nanos: u32) -> Option<Self> {
        let nanos = i128::from(secs) * 1_000_000_000 + i128::from(nanos);
        jiff::Timestamp::from_nanosecond(nanos).ok().map(Self)
    }

    pub fn from_unix_secs(secs: i64) -> Option<Self> {
        jiff::Timestamp::from_second(secs).ok().map(Self)
    }

    pub fn from_unix_micros(micros: i64) -> Option<Self> {
        jiff::Timestamp::from_microsecond(micros).ok().map(Self)
    }

    /// Whole seconds since the epoch, rounded toward the past.
    pub fn unix_secs(self) -> i64 {
        self.0.as_second() - i64::from(self.0.subsec_nanosecond() < 0)
    }

    /// Whole microseconds since the epoch, rounded toward the past.
    pub fn unix_micros(self) -> i64 {
        self.0
            .as_nanosecond()
            .div_euclid(1_000)
            .try_into()
            .unwrap_or(i64::MAX)
    }

    /// RFC 3339 with any offset, converted to UTC.
    pub fn parse_rfc3339(s: &str) -> Result<Self, jiff::Error> {
        s.parse::<jiff::Timestamp>().map(Self)
    }

    /// The calendar date in UTC.
    pub fn utc_date(self) -> jiff::civil::Date {
        jiff::tz::Offset::UTC.to_datetime(self.0).date()
    }

    /// The first instant of `date` in UTC.
    pub fn utc_midnight(date: jiff::civil::Date) -> Option<Self> {
        jiff::tz::Offset::UTC
            .to_timestamp(date.to_datetime(jiff::civil::Time::midnight()))
            .ok()
            .map(Self)
    }

    pub fn as_jiff(self) -> jiff::Timestamp {
        self.0
    }

    pub fn checked_add(self, d: SignedDuration) -> Option<Self> {
        self.0.checked_add(d).ok().map(Self)
    }

    /// `+00:00` for the offset, with the fraction as [`SecondsFormat::AutoSi`].
    pub fn to_rfc3339(self) -> String {
        self.to_rfc3339_opts(SecondsFormat::AutoSi, false)
    }

    /// `use_z` writes the offset as `Z` rather than `+00:00`.
    pub fn to_rfc3339_opts(self, seconds: SecondsFormat, use_z: bool) -> String {
        let civil = jiff::tz::Offset::UTC.to_datetime(self.0);
        let mut s = String::with_capacity(32);
        let _ = write!(
            s,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            civil.year(),
            civil.month(),
            civil.day(),
            civil.hour(),
            civil.minute(),
            civil.second(),
        );
        if seconds == SecondsFormat::AutoSi {
            let nanos = civil.subsec_nanosecond();
            if nanos == 0 {
            } else if nanos % 1_000_000 == 0 {
                let _ = write!(s, ".{:03}", nanos / 1_000_000);
            } else if nanos % 1_000 == 0 {
                let _ = write!(s, ".{:06}", nanos / 1_000);
            } else {
                let _ = write!(s, ".{nanos:09}");
            }
        }
        s.push_str(if use_z { "Z" } else { "+00:00" });
        s
    }

    /// As the serialized form, which is what a log line should show.
    fn as_text(self) -> String {
        self.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    }
}

/// A zone by its IANA name, spelled exactly as the database spells it.
///
/// The database's own lookup ignores case. Exact spelling is kept because a
/// name is stored as given and shown back, and what validation accepted
/// before -- `America/Los_Angeles`, not `america/los_angeles` -- should not
/// widen with a library.
pub fn zone(name: &str) -> Option<jiff::tz::TimeZone> {
    let tz = jiff::tz::db().get(name).ok()?;
    (tz.iana_name() == Some(name)).then_some(tz)
}

impl std::str::FromStr for Timestamp {
    type Err = jiff::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_rfc3339(s)
    }
}

impl From<jiff::Timestamp> for Timestamp {
    fn from(t: jiff::Timestamp) -> Self {
        Self(t)
    }
}

impl From<Timestamp> for jiff::Timestamp {
    fn from(t: Timestamp) -> Self {
        t.0
    }
}

/// `2026-09-18 18:00:00.123 UTC`: for people, and for the words an agent is
/// told, which said it this way before and are kept as they were.
impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self.as_text().replacen('T', " ", 1);
        write!(f, "{} UTC", text.trim_end_matches('Z'))
    }
}

impl std::fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_text())
    }
}

impl Add<SignedDuration> for Timestamp {
    type Output = Timestamp;
    /// Panics out of range, as integer overflow would.
    fn add(self, d: SignedDuration) -> Timestamp {
        self.checked_add(d).expect("timestamp in range")
    }
}

impl Sub<SignedDuration> for Timestamp {
    type Output = Timestamp;
    fn sub(self, d: SignedDuration) -> Timestamp {
        self + -d
    }
}

impl AddAssign<SignedDuration> for Timestamp {
    fn add_assign(&mut self, d: SignedDuration) {
        *self = *self + d;
    }
}

impl SubAssign<SignedDuration> for Timestamp {
    fn sub_assign(&mut self, d: SignedDuration) {
        *self = *self - d;
    }
}

impl Add<std::time::Duration> for Timestamp {
    type Output = Timestamp;
    fn add(self, d: std::time::Duration) -> Timestamp {
        self + SignedDuration::try_from(d).expect("duration in range")
    }
}

impl Sub<std::time::Duration> for Timestamp {
    type Output = Timestamp;
    fn sub(self, d: std::time::Duration) -> Timestamp {
        self - SignedDuration::try_from(d).expect("duration in range")
    }
}

impl Sub for Timestamp {
    type Output = SignedDuration;
    fn sub(self, earlier: Timestamp) -> SignedDuration {
        self.0.duration_since(earlier.0)
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_text())
    }
}

impl<'de> serde::Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::parse_rfc3339(&s).map_err(serde::de::Error::custom)
    }
}

// Postgres, by way of `time`.

fn to_offset(t: Timestamp) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(t.0.as_nanosecond())
        .expect("jiff's range lies inside time's")
}

fn from_offset(t: time::OffsetDateTime) -> Result<Timestamp, jiff::Error> {
    jiff::Timestamp::from_nanosecond(t.unix_timestamp_nanos()).map(Timestamp)
}

impl sqlx::Type<sqlx::Postgres> for Timestamp {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <time::OffsetDateTime as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <time::OffsetDateTime as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::postgres::PgHasArrayType for Timestamp {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        <time::OffsetDateTime as sqlx::postgres::PgHasArrayType>::array_type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for Timestamp {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <time::OffsetDateTime as sqlx::Encode<sqlx::Postgres>>::encode(to_offset(*self), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Timestamp {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let t = <time::OffsetDateTime as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(from_offset(t)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(nanos: i128) -> Timestamp {
        Timestamp(jiff::Timestamp::from_nanosecond(nanos).unwrap())
    }

    // What chrono wrote, so nothing already stored or sent reads differently.
    #[test]
    fn writes_what_chrono_wrote() {
        let s = 1_800_000_000i128 * 1_000_000_000;
        let json = |t: Timestamp| serde_json::to_string(&t).unwrap();
        assert_eq!(json(at(s)), "\"2027-01-15T08:00:00Z\"");
        assert_eq!(json(at(s + 120_000_000)), "\"2027-01-15T08:00:00.120Z\"");
        assert_eq!(json(at(s + 123_400_000)), "\"2027-01-15T08:00:00.123400Z\"");
        assert_eq!(json(at(s + 123_456_000)), "\"2027-01-15T08:00:00.123456Z\"");
        assert_eq!(json(at(s + 1)), "\"2027-01-15T08:00:00.000000001Z\"");
        assert_eq!(
            at(s + 5_000).to_rfc3339(),
            "2027-01-15T08:00:00.000005+00:00"
        );
        assert_eq!(
            at(s + 5_000).to_rfc3339_opts(SecondsFormat::Secs, false),
            "2027-01-15T08:00:00+00:00"
        );
        assert_eq!(
            at(s + 120_000_000).to_string(),
            "2027-01-15 08:00:00.120 UTC"
        );
        assert_eq!(json(at(-1)), "\"1969-12-31T23:59:59.999999999Z\"");
    }

    #[test]
    fn reads_any_offset() {
        let t: Timestamp = serde_json::from_str("\"2027-01-15T18:00:00.5+10:00\"").unwrap();
        assert_eq!(t, at(1_800_000_000_500_000_000));
        let back: Timestamp = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn zones_are_named_exactly() {
        assert!(zone("America/Los_Angeles").is_some());
        assert!(zone("UTC").is_some());
        assert!(zone("Europe/London").is_some());
        assert!(zone("america/los_angeles").is_none());
        assert!(zone("Mars/Olympus_Mons").is_none());
    }

    #[test]
    fn rounds_toward_the_past() {
        assert_eq!(at(-1).unix_secs(), -1);
        assert_eq!(at(-1).unix_micros(), -1);
        assert_eq!(at(1_999_999_999).unix_secs(), 1);
    }
}
