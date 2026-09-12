//! Routine timing, absence policy and firing (DOMAIN.md §13.1).
//!
//! A Routine carries `trigger {kind: cron|interval|event, spec, timezone}` and an
//! `absence_policy (skip|queue|catch_up_once)`. This module owns the minimal timing seam:
//! it computes `next_due_at` from the trigger, persists it on the canonical `routines`
//! row, and decides what a missed fire window means. Creating the Objective and its Run is
//! delegated to [`RunDispatch::fire_routine`], so firing goes through the same canonical
//! graph command path as interactive work and never writes `work_nodes`/`runs` directly.
//!
//! Timezones are UTC or a fixed `±HH:MM` offset. An IANA zone name fails closed with
//! [`SchedulerError::UnsupportedTimezone`] instead of silently scheduling in UTC; full
//! zone-database support is a later concern (it needs a tz database dependency).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Timelike, Utc};
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventError, EventStore, EventType};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use super::dispatch::{RoutineFireRequest, RunDispatch};
use super::SchedulerError;

/// How a Routine is triggered (DOMAIN.md §13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineTriggerKind {
    /// Calendar schedule.
    Cron,
    /// Fixed interval.
    Interval,
    /// External event subscription.
    Event,
}

/// A Routine trigger as persisted in `routines.trigger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineTrigger {
    /// Trigger kind.
    pub kind: RoutineTriggerKind,
    /// Cron expression (`cron`), interval (`interval`) or event name (`event`).
    pub spec: String,
    /// Timezone for `cron`, defaulting to UTC.
    #[serde(default)]
    pub timezone: String,
}

impl RoutineTrigger {
    /// The next due time strictly after `after`, or `None` for an event trigger.
    ///
    /// # Errors
    /// Returns [`SchedulerError::InvalidTrigger`] for a malformed spec and
    /// [`SchedulerError::UnsupportedTimezone`] for a zone this seam cannot resolve.
    pub fn next_due_after(
        &self,
        after: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, SchedulerError> {
        match self.kind {
            RoutineTriggerKind::Cron => {
                let offset = parse_timezone(&self.timezone)?;
                Ok(Some(self.cron()?.next_after(after, offset)?))
            }
            RoutineTriggerKind::Interval => Ok(Some(after + parse_interval(&self.spec)?)),
            RoutineTriggerKind::Event => Ok(None),
        }
    }

    /// The due time for a Routine that has never fired.
    ///
    /// # Errors
    /// See [`RoutineTrigger::next_due_after`].
    pub fn initial_due(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, SchedulerError> {
        self.next_due_after(now)
    }

    /// Validate the trigger spec up front, so a malformed Routine fails closed instead of
    /// being silently skipped.
    ///
    /// # Errors
    /// See [`RoutineTrigger::next_due_after`].
    pub fn validate(&self) -> Result<(), SchedulerError> {
        match self.kind {
            RoutineTriggerKind::Cron => {
                let _ = self.cron()?;
                let _ = parse_timezone(&self.timezone)?;
                Ok(())
            }
            RoutineTriggerKind::Interval => parse_interval(&self.spec).map(|_| ()),
            RoutineTriggerKind::Event => Ok(()),
        }
    }

    fn cron(&self) -> Result<CronSpec, SchedulerError> {
        CronSpec::parse(&self.spec)
    }
}

/// What happens to a fire window the scheduler missed (DOMAIN.md §13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbsencePolicy {
    /// Drop the missed window and resume on the next one.
    Skip,
    /// Run every missed window, oldest first, bounded by the per-tick batch.
    Queue,
    /// Run one catch-up firing for all missed windows.
    CatchUpOnce,
}

impl AbsencePolicy {
    /// The stored form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Queue => "queue",
            Self::CatchUpOnce => "catch_up_once",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`SchedulerError::InvalidConfig`] for a value the schema cannot produce.
    pub fn from_db_str(value: &str) -> Result<Self, SchedulerError> {
        match value {
            "skip" => Ok(Self::Skip),
            "queue" => Ok(Self::Queue),
            "catch_up_once" => Ok(Self::CatchUpOnce),
            other => Err(SchedulerError::InvalidConfig(format!(
                "unknown absence policy {other:?}"
            ))),
        }
    }
}

/// A persisted `routines` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Routine {
    /// Routine identity (`rtn_…`).
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Owner user.
    pub owner_user_id: String,
    /// Teammate the Routine runs as.
    pub teammate_id: Option<String>,
    /// Trigger.
    pub trigger: RoutineTrigger,
    /// Objective template plus CompletionContract.
    pub objective_template: Value,
    /// Absence policy.
    pub absence_policy: AbsencePolicy,
    /// Current status.
    pub status: String,
    /// When it last fired.
    pub last_fired_at: Option<DateTime<Utc>>,
    /// When it is next due.
    pub next_due_at: Option<DateTime<Utc>>,
}

/// Result of one routine pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoutineTickReport {
    /// Windows fired (Objectives created).
    pub fired: usize,
    /// Windows dropped by the `skip` policy.
    pub skipped: usize,
    /// Fired windows that came from a backlog under the `queue` policy.
    pub queued: usize,
}

/// Routine timing store for one tenant.
#[derive(Debug, Clone)]
pub struct RoutineScheduler {
    events: EventStore,
    tenant_id: String,
}

impl RoutineScheduler {
    /// Bind a routine scheduler to a tenant.
    ///
    /// # Errors
    /// Returns a schema error when the tenant id is not a canonical `tn_` id.
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Result<Self, SchedulerError> {
        let tenant_id = tenant_id.into();
        crate::control::schema::validate_tenant_id(&tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool),
            tenant_id,
        })
    }

    /// Number of active routines whose window has passed.
    ///
    /// # Errors
    /// Returns a scheduler error when the count query fails.
    pub async fn due_count(&self, now: DateTime<Utc>) -> Result<i64, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM routines WHERE tenant_id = $1 AND status = 'active' \
             AND next_due_at IS NOT NULL AND next_due_at <= $2",
        )
        .bind(&self.tenant_id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(count)
    }

    /// Active routines whose window has passed, oldest first.
    ///
    /// # Errors
    /// Returns a scheduler error when the query fails or a row cannot be decoded.
    pub async fn due(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<Routine>, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(
            "SELECT id, tenant_id, workspace_id, owner_user_id, teammate_id, trigger, \
             objective_template, absence_policy, status, last_fired_at, next_due_at \
             FROM routines WHERE tenant_id = $1 AND status = 'active' \
             AND next_due_at IS NOT NULL AND next_due_at <= $2 \
             ORDER BY next_due_at ASC, id LIMIT $3",
        )
        .bind(&self.tenant_id)
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let routines = rows
            .iter()
            .map(routine_from_row)
            .collect::<Result<Vec<_>, SchedulerError>>()?;
        tx.commit().await?;
        Ok(routines)
    }

    /// Apply the absence policy to every due Routine in this tick.
    ///
    /// # Errors
    /// Returns a scheduler error when a trigger is malformed, the graph refuses a firing,
    /// or the bookkeeping transaction fails.
    pub async fn tick(
        &self,
        dispatch: &dyn RunDispatch,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<RoutineTickReport, SchedulerError> {
        let mut report = RoutineTickReport::default();
        for routine in self.due(now, limit).await? {
            routine.trigger.validate()?;
            let window = routine
                .next_due_at
                .ok_or_else(|| SchedulerError::InvalidTrigger {
                    spec: routine.trigger.spec.clone(),
                    reason: "due routine has no next_due_at".to_string(),
                })?;

            match routine.absence_policy {
                AbsencePolicy::Skip => {
                    let next = routine.trigger.next_due_after(now)?;
                    self.record(
                        &routine,
                        next,
                        None,
                        "routine.skipped",
                        json!({ "window": window.to_rfc3339(), "reason": "missed_window" }),
                    )
                    .await?;
                    report.skipped += 1;
                }
                AbsencePolicy::Queue => {
                    // Fire the oldest missed window and leave the next one due, so a
                    // backlog drains one window per tick instead of silently collapsing.
                    let next = routine.trigger.next_due_after(window)?;
                    let fired = dispatch
                        .fire_routine(self.fire_request(&routine, window))
                        .await?;
                    self.record(
                        &routine,
                        next,
                        Some(now),
                        "routine.fired",
                        json!({
                            "window": window.to_rfc3339(),
                            "objective_id": fired.objective_id,
                            "run_id": fired.run_id,
                        }),
                    )
                    .await?;
                    report.fired += 1;
                    report.queued += 1;
                }
                AbsencePolicy::CatchUpOnce => {
                    let next = routine.trigger.next_due_after(now)?;
                    let fired = dispatch
                        .fire_routine(self.fire_request(&routine, window))
                        .await?;
                    self.record(
                        &routine,
                        next,
                        Some(now),
                        "routine.fired",
                        json!({
                            "window": window.to_rfc3339(),
                            "objective_id": fired.objective_id,
                            "run_id": fired.run_id,
                        }),
                    )
                    .await?;
                    report.fired += 1;
                }
            }
        }
        Ok(report)
    }

    fn fire_request(&self, routine: &Routine, window: DateTime<Utc>) -> RoutineFireRequest {
        RoutineFireRequest {
            tenant_id: self.tenant_id.clone(),
            workspace_id: routine.workspace_id.clone(),
            routine_id: routine.id.clone(),
            owner_user_id: routine.owner_user_id.clone(),
            teammate_id: routine.teammate_id.clone(),
            objective_template: routine.objective_template.clone(),
            fire_window: window,
            fire_key: format!("routine:{}:{}", routine.id, window.to_rfc3339()),
        }
    }

    /// Persist the next due time and emit the canonical routine event atomically.
    async fn record(
        &self,
        routine: &Routine,
        next_due_at: Option<DateTime<Utc>>,
        last_fired_at: Option<DateTime<Utc>>,
        event_type: &str,
        payload: Value,
    ) -> Result<(), SchedulerError> {
        let tenant_id = self.tenant_id.clone();
        let routine_id = routine.id.clone();
        let workspace_id = routine.workspace_id.clone();
        let event_type = EventType::parse(event_type)?;
        let abort = Arc::new(AtomicBool::new(false));
        let abort_in = Arc::clone(&abort);

        let result = self
            .events
            .commit_mutation(&self.tenant_id, |conn, batch| {
                Box::pin(async move {
                    let updated = sqlx::query(
                        "UPDATE routines SET next_due_at = $1, \
                         last_fired_at = COALESCE($2, last_fired_at) \
                         WHERE id = $3 AND tenant_id = $4 AND status = 'active'",
                    )
                    .bind(next_due_at)
                    .bind(last_fired_at)
                    .bind(&routine_id)
                    .bind(&tenant_id)
                    .execute(&mut *conn)
                    .await?;
                    if updated.rows_affected() != 1 {
                        abort_in.store(true, Ordering::SeqCst);
                        return Ok(());
                    }
                    let mut generator = UlidGenerator::new();
                    let draft = EventDraft::new(
                        "routine",
                        routine_id.clone(),
                        1,
                        event_type.clone(),
                        CorrelationId::generate(&mut generator),
                        Actor::system("scheduler"),
                    )
                    .with_workspace(workspace_id.clone())
                    .with_payload(payload.clone());
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await;

        let aborted = abort.load(Ordering::SeqCst);
        match (result, aborted) {
            (Ok(()), false) => Ok(()),
            (Err(EventError::NoEventStaged), true) => Err(SchedulerError::RoutineNotActive {
                routine_id: routine.id.clone(),
            }),
            (Err(error), _) => Err(error.into()),
            (Ok(()), true) => Err(SchedulerError::RoutineNotActive {
                routine_id: routine.id.clone(),
            }),
        }
    }
}

fn routine_from_row(row: &sqlx::postgres::PgRow) -> Result<Routine, SchedulerError> {
    let trigger: Value = row.try_get("trigger")?;
    let trigger: RoutineTrigger =
        serde_json::from_value(trigger).map_err(|error| SchedulerError::InvalidTrigger {
            spec: "routines.trigger".to_string(),
            reason: error.to_string(),
        })?;
    let policy: String = row.try_get("absence_policy")?;
    Ok(Routine {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        owner_user_id: row.try_get("owner_user_id")?,
        teammate_id: row.try_get("teammate_id")?,
        trigger,
        objective_template: row.try_get("objective_template")?,
        absence_policy: AbsencePolicy::from_db_str(&policy)?,
        status: row.try_get("status")?,
        last_fired_at: row.try_get("last_fired_at")?,
        next_due_at: row.try_get("next_due_at")?,
    })
}

/// Parse `30s`/`5m`/`2h`/`1d` or a plain number of seconds.
fn parse_interval(spec: &str) -> Result<Duration, SchedulerError> {
    let invalid = |reason: &str| SchedulerError::InvalidTrigger {
        spec: spec.to_string(),
        reason: reason.to_string(),
    };
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(invalid("empty interval"));
    }
    if let Ok(seconds) = spec.parse::<i64>() {
        return positive_interval(seconds, &invalid);
    }
    let (value, unit) = spec.split_at(spec.len() - 1);
    let value: i64 = value.parse().map_err(|_| invalid("expected a number"))?;
    let seconds = match unit {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        "d" => value * 86_400,
        _ => return Err(invalid("expected a suffix of s, m, h or d")),
    };
    positive_interval(seconds, &invalid)
}

fn positive_interval(
    seconds: i64,
    invalid: &dyn Fn(&str) -> SchedulerError,
) -> Result<Duration, SchedulerError> {
    if seconds <= 0 {
        return Err(invalid("interval must be positive"));
    }
    Ok(Duration::seconds(seconds))
}

/// Parse `UTC`, `Z` or a fixed `±HH:MM` offset. Anything else fails closed.
fn parse_timezone(timezone: &str) -> Result<FixedOffset, SchedulerError> {
    let unsupported = || SchedulerError::UnsupportedTimezone(timezone.to_string());
    let tz = timezone.trim();
    if tz.is_empty() || tz.eq_ignore_ascii_case("utc") || tz.eq_ignore_ascii_case("z") {
        return FixedOffset::east_opt(0).ok_or_else(unsupported);
    }
    let bytes = tz.as_bytes();
    if bytes.len() < 3 || (bytes[0] != b'+' && bytes[0] != b'-') {
        return Err(unsupported());
    }
    let sign = if bytes[0] == b'+' { 1 } else { -1 };
    let rest = &tz[1..];
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if rest.len() == 4 => (&rest[..2], &rest[2..]),
        None => (rest, "0"),
    };
    let hours: i32 = hours.parse().map_err(|_| unsupported())?;
    let minutes: i32 = minutes.parse().map_err(|_| unsupported())?;
    if hours > 23 || minutes > 59 {
        return Err(unsupported());
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60)).ok_or_else(unsupported)
}

/// A parsed five-field cron expression: `minute hour day-of-month month day-of-week`.
#[derive(Debug, Clone)]
struct CronSpec {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>,
    day_of_month_any: bool,
    day_of_week_any: bool,
}

impl CronSpec {
    fn parse(spec: &str) -> Result<Self, SchedulerError> {
        let fields: Vec<&str> = spec.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(SchedulerError::InvalidTrigger {
                spec: spec.to_string(),
                reason: "expected five fields: minute hour day-of-month month day-of-week"
                    .to_string(),
            });
        }
        let (minutes, _) = parse_cron_field(fields[0], 0, 59, spec)?;
        let (hours, _) = parse_cron_field(fields[1], 0, 23, spec)?;
        let (days_of_month, day_of_month_any) = parse_cron_field(fields[2], 1, 31, spec)?;
        let (months, _) = parse_cron_field(fields[3], 1, 12, spec)?;
        let (mut days_of_week, day_of_week_any) = parse_cron_field(fields[4], 0, 7, spec)?;
        for day in &mut days_of_week {
            if *day == 7 {
                *day = 0;
            }
        }
        days_of_week.sort_unstable();
        days_of_week.dedup();
        Ok(Self {
            minutes,
            hours,
            days_of_month,
            months,
            days_of_week,
            day_of_month_any,
            day_of_week_any,
        })
    }

    fn matches(&self, candidate: chrono::NaiveDateTime) -> bool {
        let day_of_week = candidate.weekday().num_days_from_sunday();
        let day_of_month = candidate.day();
        let day_matches = match (self.day_of_month_any, self.day_of_week_any) {
            (true, true) => true,
            (false, true) => self.days_of_month.contains(&day_of_month),
            (true, false) => self.days_of_week.contains(&day_of_week),
            // Standard cron: when both are restricted, either may match.
            (false, false) => {
                self.days_of_month.contains(&day_of_month)
                    || self.days_of_week.contains(&day_of_week)
            }
        };
        day_matches
            && self.minutes.contains(&candidate.minute())
            && self.hours.contains(&candidate.hour())
            && self.months.contains(&candidate.month())
    }

    fn next_after(
        &self,
        after: DateTime<Utc>,
        offset: FixedOffset,
    ) -> Result<DateTime<Utc>, SchedulerError> {
        let mut candidate = offset
            .from_utc_datetime(&after.naive_utc())
            .naive_local()
            .with_second(0)
            .and_then(|value| value.with_nanosecond(0))
            .unwrap_or_else(|| after.naive_utc())
            + Duration::minutes(1);
        let limit = candidate + Duration::days(366);
        while candidate <= limit {
            if self.matches(candidate) {
                if let Some(next) = offset.from_local_datetime(&candidate).single() {
                    return Ok(next.with_timezone(&Utc));
                }
            }
            candidate += Duration::minutes(1);
        }
        Err(SchedulerError::InvalidTrigger {
            spec: "cron".to_string(),
            reason: "no occurrence within 366 days".to_string(),
        })
    }
}

fn parse_cron_field(
    field: &str,
    min: u32,
    max: u32,
    spec: &str,
) -> Result<(Vec<u32>, bool), SchedulerError> {
    let invalid = |reason: &str| SchedulerError::InvalidTrigger {
        spec: spec.to_string(),
        reason: reason.to_string(),
    };
    let field = field.trim();
    if field.is_empty() {
        return Err(invalid("empty field"));
    }
    if field == "*" {
        return Ok(((min..=max).collect(), true));
    }
    let mut values = std::collections::BTreeSet::new();
    for part in field.split(',') {
        let part = part.trim();
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (
                range,
                step.parse::<u32>().map_err(|_| invalid("invalid step"))?,
            ),
            None => (part, 1),
        };
        if step == 0 {
            return Err(invalid("step must be positive"));
        }
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((start, end)) = range.split_once('-') {
            (
                start.parse::<u32>().map_err(|_| invalid("invalid range"))?,
                end.parse::<u32>().map_err(|_| invalid("invalid range"))?,
            )
        } else {
            let value = range.parse::<u32>().map_err(|_| invalid("invalid value"))?;
            (value, value)
        };
        if start > end || start < min || end > max {
            return Err(invalid("value out of range"));
        }
        let mut value = start;
        while value <= end {
            values.insert(value);
            value += step;
        }
    }
    Ok((values.into_iter().collect(), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(kind: RoutineTriggerKind, spec: &str, timezone: &str) -> RoutineTrigger {
        RoutineTrigger {
            kind,
            spec: spec.to_string(),
            timezone: timezone.to_string(),
        }
    }

    #[test]
    fn interval_specs_parse_to_positive_durations() {
        assert_eq!(
            parse_interval("90").expect("seconds"),
            Duration::seconds(90)
        );
        assert_eq!(
            parse_interval("30s").expect("seconds"),
            Duration::seconds(30)
        );
        assert_eq!(
            parse_interval("5m").expect("minutes"),
            Duration::seconds(300)
        );
        assert_eq!(
            parse_interval("2h").expect("hours"),
            Duration::seconds(7200)
        );
        assert_eq!(
            parse_interval("1d").expect("days"),
            Duration::seconds(86_400)
        );
        assert!(parse_interval("0s").is_err());
        assert!(parse_interval("5x").is_err());
        assert!(parse_interval("").is_err());
    }

    #[test]
    fn timezones_are_utc_or_fixed_offsets_and_fail_closed_otherwise() {
        assert_eq!(
            parse_timezone("UTC").expect("utc"),
            FixedOffset::east_opt(0).unwrap()
        );
        assert_eq!(
            parse_timezone("").expect("default"),
            FixedOffset::east_opt(0).unwrap()
        );
        assert_eq!(
            parse_timezone("+02:00").expect("offset"),
            FixedOffset::east_opt(7200).unwrap()
        );
        assert_eq!(
            parse_timezone("-0530").expect("offset"),
            FixedOffset::east_opt(-(5 * 3600 + 30 * 60)).unwrap()
        );
        assert!(matches!(
            parse_timezone("America/New_York"),
            Err(SchedulerError::UnsupportedTimezone(_))
        ));
        assert!(parse_timezone("+25:00").is_err());
    }

    #[test]
    fn cron_next_due_respects_fields_and_timezone() {
        let every_five = trigger(RoutineTriggerKind::Cron, "*/5 * * * *", "UTC").next_due_after(
            DateTime::parse_from_rfc3339("2026-01-01T00:03:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert_eq!(
            every_five.expect("next"),
            Some(
                DateTime::parse_from_rfc3339("2026-01-01T00:05:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );

        // 09:00 in +02:00 is 07:00 UTC.
        let nine_local = trigger(RoutineTriggerKind::Cron, "0 9 * * *", "+02:00")
            .next_due_after(
                DateTime::parse_from_rfc3339("2026-01-01T06:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .expect("next");
        assert_eq!(
            nine_local,
            Some(
                DateTime::parse_from_rfc3339("2026-01-01T07:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );

        // Weekday-only schedule: 2026-01-03 is a Saturday, so the next weekday is Monday.
        let weekdays = trigger(RoutineTriggerKind::Cron, "0 9 * * 1-5", "UTC")
            .next_due_after(
                DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .expect("next");
        assert_eq!(
            weekdays,
            Some(
                DateTime::parse_from_rfc3339("2026-01-05T09:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );

        assert!(trigger(RoutineTriggerKind::Cron, "not a cron", "UTC")
            .validate()
            .is_err());
        assert!(trigger(RoutineTriggerKind::Cron, "* * * *", "UTC")
            .validate()
            .is_err());
    }

    #[test]
    fn event_triggers_have_no_due_time_and_intervals_advance() {
        let event = trigger(RoutineTriggerKind::Event, "connector.github.push", "UTC");
        assert_eq!(event.next_due_after(Utc::now()).expect("event"), None);
        let interval = trigger(RoutineTriggerKind::Interval, "5m", "UTC");
        let after = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            interval.next_due_after(after).expect("interval"),
            Some(after + Duration::minutes(5))
        );
    }

    #[test]
    fn absence_policies_round_trip_through_the_stored_form() {
        for policy in [
            AbsencePolicy::Skip,
            AbsencePolicy::Queue,
            AbsencePolicy::CatchUpOnce,
        ] {
            assert_eq!(
                AbsencePolicy::from_db_str(policy.as_db_str()).expect("policy"),
                policy
            );
        }
        assert!(AbsencePolicy::from_db_str("explode").is_err());
    }
}
