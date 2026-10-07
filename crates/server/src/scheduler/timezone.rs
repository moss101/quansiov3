//! Schedule time zones with explicit daylight-saving rules (ADP-007, DOMAIN.md §13.8 C).
//!
//! A routine's `timezone` is `UTC` (or empty), a fixed `±HH:MM` offset, or an IANA zone name
//! (`America/New_York`). The zone rules come from the tz database that ships inside the
//! `chrono-tz` crate, so a schedule means the same thing on every host. A name that is none
//! of those fails closed with [`SchedulerError::UnknownTimezone`]; nothing is guessed and
//! nothing falls back to UTC.
//!
//! A wall-clock time a zone moves across is resolved by two fixed rules:
//!
//! * **A time that does not exist** (the hour skipped when clocks go forward) fires at the
//!   first valid instant after the gap — the moment the clocks jump — and fires once, however
//!   many of the schedule's minutes fell in the gap.
//! * **A time that happens twice** (the hour repeated when clocks go back) fires once, at its
//!   first occurrence; the second occurrence of the same wall-clock time is not a window.

use std::str::FromStr;

use chrono::{DateTime, Duration, FixedOffset, LocalResult, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;

use super::SchedulerError;

/// A zone a schedule is evaluated in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleZone {
    /// A fixed offset from UTC (UTC itself is offset zero).
    Fixed(FixedOffset),
    /// An IANA zone with its transitions.
    Iana(Tz),
}

impl ScheduleZone {
    /// Parse a routine's `timezone` field.
    ///
    /// # Errors
    /// Returns [`SchedulerError::UnknownTimezone`] for anything that is not UTC, a fixed
    /// offset within ±23:59, or a name in the IANA database.
    pub fn parse(timezone: &str) -> Result<Self, SchedulerError> {
        let unknown = || SchedulerError::UnknownTimezone(timezone.to_string());
        let tz = timezone.trim();
        if tz.is_empty() || tz.eq_ignore_ascii_case("utc") || tz.eq_ignore_ascii_case("z") {
            return FixedOffset::east_opt(0)
                .map(Self::Fixed)
                .ok_or_else(unknown);
        }
        let bytes = tz.as_bytes();
        if bytes[0] == b'+' || bytes[0] == b'-' {
            return parse_offset(tz).map(Self::Fixed).ok_or_else(unknown);
        }
        // The database is case sensitive and has no spaces; reject what it cannot name.
        if tz.len() > 64
            || !tz.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'+')
            })
        {
            return Err(unknown());
        }
        Tz::from_str(tz).map(Self::Iana).map_err(|_| unknown())
    }

    /// The wall-clock reading of an instant in this zone.
    #[must_use]
    pub fn local(&self, instant: DateTime<Utc>) -> NaiveDateTime {
        match self {
            Self::Fixed(offset) => offset.from_utc_datetime(&instant.naive_utc()).naive_local(),
            Self::Iana(zone) => zone.from_utc_datetime(&instant.naive_utc()).naive_local(),
        }
    }

    /// The instant a wall-clock time falls at, by the two rules in the module comment.
    ///
    /// `None` only for a time more than a day away from any valid one, which no zone in the
    /// database has.
    #[must_use]
    pub fn resolve(&self, local: NaiveDateTime) -> Option<DateTime<Utc>> {
        match self.map_local(local) {
            LocalResult::Single(instant) => Some(instant),
            // Happens twice: the first occurrence.
            LocalResult::Ambiguous(first, second) => Some(first.min(second)),
            // Does not exist: the first valid instant after the gap.
            LocalResult::None => self.gap_end(local),
        }
    }

    /// Whether `local` is a wall-clock time this zone skips.
    #[must_use]
    pub fn is_skipped(&self, local: NaiveDateTime) -> bool {
        matches!(self.map_local(local), LocalResult::None)
    }

    /// The offset from UTC the zone has at `instant`, in seconds east.
    #[must_use]
    pub fn offset_seconds_at(&self, instant: DateTime<Utc>) -> i32 {
        match self {
            Self::Fixed(offset) => offset.local_minus_utc(),
            Self::Iana(zone) => zone
                .offset_from_utc_datetime(&instant.naive_utc())
                .fix()
                .local_minus_utc(),
        }
    }

    fn map_local(&self, local: NaiveDateTime) -> LocalResult<DateTime<Utc>> {
        match self {
            Self::Fixed(offset) => offset
                .from_local_datetime(&local)
                .map(|at| at.with_timezone(&Utc)),
            Self::Iana(zone) => zone
                .from_local_datetime(&local)
                .map(|at| at.with_timezone(&Utc)),
        }
    }

    /// The first instant after the gap that contains `local`: walk the wall clock forward a
    /// minute at a time until it reads a time the zone has.
    fn gap_end(&self, local: NaiveDateTime) -> Option<DateTime<Utc>> {
        let mut probe = local;
        // The longest skip in the database is a day (a zone moving across the date line);
        // two days is a bound that cannot be reached by a real zone.
        for _ in 0..(2 * 24 * 60) {
            probe += Duration::minutes(1);
            match self.map_local(probe) {
                LocalResult::Single(instant) => return Some(instant),
                LocalResult::Ambiguous(first, second) => return Some(first.min(second)),
                LocalResult::None => {}
            }
        }
        None
    }
}

/// `+HH:MM`, `-HHMM`, `+HH`: a fixed offset within ±23:59.
fn parse_offset(tz: &str) -> Option<FixedOffset> {
    let bytes = tz.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let sign = if bytes[0] == b'+' { 1 } else { -1 };
    let rest = &tz[1..];
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if rest.len() == 4 => (&rest[..2], &rest[2..]),
        None => (rest, "0"),
    };
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("instant")
            .with_timezone(&Utc)
    }

    fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|date| date.and_hms_opt(hour, minute, 0))
            .expect("local")
    }

    #[test]
    fn names_are_utc_offsets_or_the_iana_database_and_nothing_else() {
        assert_eq!(
            ScheduleZone::parse("").expect("empty"),
            ScheduleZone::Fixed(FixedOffset::east_opt(0).expect("zero"))
        );
        assert_eq!(
            ScheduleZone::parse("+02:00").expect("offset"),
            ScheduleZone::Fixed(FixedOffset::east_opt(7200).expect("two hours"))
        );
        assert_eq!(
            ScheduleZone::parse("-0530").expect("offset"),
            ScheduleZone::Fixed(FixedOffset::east_opt(-(5 * 3600 + 30 * 60)).expect("offset"))
        );
        assert!(matches!(
            ScheduleZone::parse("America/New_York"),
            Ok(ScheduleZone::Iana(_))
        ));
        assert!(matches!(
            ScheduleZone::parse("Australia/Lord_Howe"),
            Ok(ScheduleZone::Iana(_))
        ));
        for bad in [
            "Mars/Olympus_Mons",
            "america/new_york",
            "New York",
            "+25:00",
            "+02:60",
            "Europe/",
            "../etc/passwd",
            "UTC+2",
            &"A".repeat(80),
        ] {
            assert!(
                matches!(
                    ScheduleZone::parse(bad),
                    Err(SchedulerError::UnknownTimezone(_))
                ),
                "{bad:?} must fail closed"
            );
        }
    }

    #[test]
    fn a_skipped_wall_clock_time_resolves_to_the_end_of_the_gap() {
        let new_york = ScheduleZone::parse("America/New_York").expect("zone");
        // 2026-03-08: clocks go from 02:00 EST to 03:00 EDT; 02:30 does not exist.
        assert!(new_york.is_skipped(local(2026, 3, 8, 2, 30)));
        assert_eq!(
            new_york.resolve(local(2026, 3, 8, 2, 30)),
            Some(at("2026-03-08T07:00:00Z")),
            "02:30 fires at 03:00 EDT, the first valid instant after the gap"
        );
        // The first minute of the gap and its last minute resolve to the same instant.
        assert_eq!(
            new_york.resolve(local(2026, 3, 8, 2, 0)),
            Some(at("2026-03-08T07:00:00Z"))
        );
        assert_eq!(
            new_york.resolve(local(2026, 3, 8, 2, 59)),
            Some(at("2026-03-08T07:00:00Z"))
        );
        // The minute before and after the gap are ordinary.
        assert_eq!(
            new_york.resolve(local(2026, 3, 8, 1, 59)),
            Some(at("2026-03-08T06:59:00Z"))
        );
        assert_eq!(
            new_york.resolve(local(2026, 3, 8, 3, 0)),
            Some(at("2026-03-08T07:00:00Z"))
        );
        // A half-hour gap (Lord Howe, 2026-10-04: 02:00 +10:30 to 02:30 +11:00).
        let lord_howe = ScheduleZone::parse("Australia/Lord_Howe").expect("zone");
        assert!(lord_howe.is_skipped(local(2026, 10, 4, 2, 15)));
        assert_eq!(
            lord_howe.resolve(local(2026, 10, 4, 2, 15)),
            Some(at("2026-10-03T15:30:00Z")),
            "02:15 fires at 02:30 +11:00"
        );
    }

    #[test]
    fn a_repeated_wall_clock_time_resolves_to_its_first_occurrence() {
        let new_york = ScheduleZone::parse("America/New_York").expect("zone");
        // 2026-11-01: clocks go from 02:00 EDT back to 01:00 EST; 01:30 happens twice.
        assert!(!new_york.is_skipped(local(2026, 11, 1, 1, 30)));
        assert_eq!(
            new_york.resolve(local(2026, 11, 1, 1, 30)),
            Some(at("2026-11-01T05:30:00Z")),
            "the EDT occurrence, not the EST one an hour later"
        );
        let london = ScheduleZone::parse("Europe/London").expect("zone");
        // 2026-10-25: 02:00 BST back to 01:00 GMT.
        assert_eq!(
            london.resolve(local(2026, 10, 25, 1, 30)),
            Some(at("2026-10-25T00:30:00Z"))
        );
    }

    #[test]
    fn the_offset_follows_the_transitions() {
        let new_york = ScheduleZone::parse("America/New_York").expect("zone");
        assert_eq!(
            new_york.offset_seconds_at(at("2026-01-15T12:00:00Z")),
            -5 * 3600
        );
        assert_eq!(
            new_york.offset_seconds_at(at("2026-07-15T12:00:00Z")),
            -4 * 3600
        );
        let kolkata = ScheduleZone::parse("Asia/Kolkata").expect("zone");
        assert_eq!(
            kolkata.offset_seconds_at(at("2026-07-15T12:00:00Z")),
            19_800
        );
        assert_eq!(
            new_york.local(at("2026-03-08T06:59:00Z")),
            local(2026, 3, 8, 1, 59)
        );
        assert_eq!(
            new_york.local(at("2026-03-08T07:00:00Z")),
            local(2026, 3, 8, 3, 0)
        );
    }
}
