//! Routine timing, absence policy and firing (DOMAIN.md §13.1).
//!
//! A Routine carries `trigger {kind: cron|interval|event, spec, timezone}` and an
//! `absence_policy (skip|queue|catch_up_once)`. This module owns the minimal timing seam:
//! it computes `next_due_at` from the trigger, persists it on the canonical `routines`
//! row, and decides what a missed fire window means. Creating the Objective and its Run is
//! delegated to [`RunDispatch::fire_routine`], so firing goes through the same canonical
//! graph command path as interactive work and never writes `work_nodes`/`runs` directly.
//!
//! A fired window is *claimed* in the firing ledger (`control::routines::ledger`) in the
//! same transaction that advances `next_due_at` and emits `routine.fired`, and only then
//! executed: executing leases the fire, so a restart between claim and execution resumes
//! the fire instead of repeating the window, and a failed execution is retried with
//! backoff (APP-010).
//!
//! Timezones are UTC, a fixed `±HH:MM` offset or an IANA zone name with its daylight-saving
//! rules ([`super::timezone`], ADP-007): a wall-clock time a zone skips fires at the first
//! valid instant after the gap, one a zone repeats fires once at its first occurrence, and a
//! name that is none of these fails closed with [`SchedulerError::UnknownTimezone`] instead of
//! silently scheduling in UTC.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use futures_util::StreamExt;
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventError, EventStore, EventType};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use super::dispatch::{RoutineFireRequest, RunDispatch};
use super::timezone::ScheduleZone;
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
    /// An inbound hook: a signed HTTP request to the routine's endpoint (ADP-007).
    Webhook,
}

/// A Routine trigger as persisted in `routines.trigger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineTrigger {
    /// Trigger kind.
    pub kind: RoutineTriggerKind,
    /// Cron expression (`cron`), interval (`interval`) or event name (`event`); empty for a `webhook`.
    #[serde(default)]
    pub spec: String,
    /// Timezone for `cron`, defaulting to UTC.
    #[serde(default)]
    pub timezone: String,
    /// For `webhook`: the groups of conditions a delivery's payload must satisfy, any group (OR) of every
    /// condition in it (AND). Empty passes every authenticated delivery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<Vec<crate::control::routines::hooks::HookCondition>>,
}

impl RoutineTrigger {
    /// The next due time strictly after `after`, or `None` for an event trigger.
    ///
    /// # Errors
    /// Returns [`SchedulerError::InvalidTrigger`] for a malformed spec and
    /// [`SchedulerError::UnknownTimezone`] for a zone this seam cannot resolve.
    pub fn next_due_after(
        &self,
        after: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, SchedulerError> {
        match self.kind {
            RoutineTriggerKind::Cron => {
                let zone = ScheduleZone::parse(&self.timezone)?;
                Ok(Some(self.cron()?.next_after(after, zone)?))
            }
            RoutineTriggerKind::Interval => Ok(Some(after + parse_interval(&self.spec)?)),
            RoutineTriggerKind::Event | RoutineTriggerKind::Webhook => Ok(None),
        }
    }

    /// The shortest gap between this schedule's next `windows` windows after `from`: the interval itself for an interval
    /// trigger, the smallest of the consecutive gaps for a cron trigger, `None` for a trigger with no windows.
    ///
    /// # Errors
    /// See [`RoutineTrigger::next_due_after`].
    pub fn shortest_gap(
        &self,
        from: DateTime<Utc>,
        windows: usize,
    ) -> Result<Option<Duration>, SchedulerError> {
        match self.kind {
            RoutineTriggerKind::Interval => Ok(Some(parse_interval(&self.spec)?)),
            RoutineTriggerKind::Cron => {
                let mut previous = from;
                let mut shortest: Option<Duration> = None;
                for _ in 0..windows {
                    let Some(next) = self.next_due_after(previous)? else {
                        break;
                    };
                    if previous != from {
                        let gap = next - previous;
                        shortest = Some(shortest.map_or(gap, |known| known.min(gap)));
                    }
                    previous = next;
                }
                Ok(shortest)
            }
            RoutineTriggerKind::Event | RoutineTriggerKind::Webhook => Ok(None),
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
                let _ = ScheduleZone::parse(&self.timezone)?;
                Ok(())
            }
            RoutineTriggerKind::Interval => parse_interval(&self.spec).map(|_| ()),
            RoutineTriggerKind::Event | RoutineTriggerKind::Webhook => Ok(()),
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
    /// Claimed fires executed to a run this tick.
    pub executed: usize,
    /// Fire executions that failed this tick (retried later, or failed for good).
    pub execution_failures: usize,
}

/// How late the scheduler may reach a window and still fire it as on time (DOMAIN.md §13.1).
pub const DEFAULT_MISFIRE_THRESHOLD_SECONDS: i64 = 60;
/// Leased fires one tenant pass executes side by side.
pub const DEFAULT_CONCURRENT_FIRES: usize = 8;

/// Routine timing store for one tenant.
#[derive(Debug, Clone)]
pub struct RoutineScheduler {
    events: EventStore,
    tenant_id: String,
    misfire_threshold: Duration,
    concurrent_fires: usize,
    execute_fires: bool,
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
            misfire_threshold: Duration::seconds(DEFAULT_MISFIRE_THRESHOLD_SECONDS),
            concurrent_fires: DEFAULT_CONCURRENT_FIRES,
            execute_fires: true,
        })
    }

    /// Claim due windows in [`Self::tick`] but leave executing the fires to a
    /// [`super::FireExecutor`].
    #[must_use]
    pub fn claims_only(mut self) -> Self {
        self.execute_fires = false;
        self
    }

    /// Use `threshold` instead of the default misfire threshold.
    #[must_use]
    pub fn with_misfire_threshold(mut self, threshold: Duration) -> Self {
        self.misfire_threshold = threshold;
        self
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

            // The latest due window, reached within the misfire threshold, is on time and fires
            // whatever the policy; the absence policy decides only what a window reached late (or
            // followed by another window that has also come due) means.
            let following = routine.trigger.next_due_after(window)?;
            let on_time =
                now - window <= self.misfire_threshold && following.is_none_or(|next| next > now);
            if on_time {
                if self.claim(&routine, window, following, now).await? {
                    report.fired += 1;
                }
                continue;
            }

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
                    // Claim the oldest missed window and leave the next one due, so a
                    // backlog drains one window per tick instead of silently collapsing.
                    let next = routine.trigger.next_due_after(window)?;
                    if self.claim(&routine, window, next, now).await? {
                        report.fired += 1;
                        report.queued += 1;
                    }
                }
                AbsencePolicy::CatchUpOnce => {
                    let next = routine.trigger.next_due_after(now)?;
                    if self.claim(&routine, window, next, now).await? {
                        report.fired += 1;
                    }
                }
            }
        }
        let (executed, failures) = if self.execute_fires {
            self.execute_pending(dispatch, now, limit).await?
        } else {
            (0, 0)
        };
        report.executed = executed;
        report.execution_failures = failures;
        Ok(report)
    }

    fn fire_request(
        &self,
        routine: &Routine,
        window: DateTime<Utc>,
        fire_key: &str,
        payload: Option<Value>,
    ) -> RoutineFireRequest {
        RoutineFireRequest {
            tenant_id: self.tenant_id.clone(),
            workspace_id: routine.workspace_id.clone(),
            routine_id: routine.id.clone(),
            owner_user_id: routine.owner_user_id.clone(),
            teammate_id: routine.teammate_id.clone(),
            objective_template: routine.objective_template.clone(),
            fire_window: window,
            fire_key: fire_key.to_string(),
            payload,
        }
    }

    /// Claim `window` in the firing ledger and advance the routine, in one transaction.
    /// Returns `false` when another instance already claimed it.
    async fn claim(
        &self,
        routine: &Routine,
        window: DateTime<Utc>,
        next_due_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, SchedulerError> {
        let tenant_id = self.tenant_id.clone();
        let routine_id = routine.id.clone();
        let workspace_id = routine.workspace_id.clone();
        let key = format!("routine:{}:{}", routine.id, window.to_rfc3339());
        let result = self
            .events
            .commit_mutation(&self.tenant_id, move |conn, batch| {
                Box::pin(async move {
                    // Guarded by the window this instance read: a concurrent instance that
                    // advanced the routine first leaves nothing to update here.
                    let updated = sqlx::query(
                        "UPDATE routines SET next_due_at = $1, last_fired_at = $2 \
                         WHERE id = $3 AND tenant_id = $4 AND status = 'active' AND next_due_at = $5",
                    )
                    .bind(next_due_at)
                    .bind(now)
                    .bind(&routine_id)
                    .bind(&tenant_id)
                    .bind(window)
                    .execute(&mut *conn)
                    .await?;
                    if updated.rows_affected() != 1 {
                        return Ok(false);
                    }
                    Ok(crate::control::routines::ledger::claim_in(
                        conn,
                        batch,
                        &tenant_id,
                        &workspace_id,
                        &routine_id,
                        &key,
                        crate::control::routines::FireSource::Schedule,
                        Some(window),
                        None,
                        None,
                    )
                    .await?
                    .is_some())
                })
            })
            .await;
        match result {
            Ok(claimed) => Ok(claimed),
            // Nothing staged: the window was claimed (or the routine changed) elsewhere, and
            // the transaction, including its update, was rolled back.
            Err(EventError::NoEventStaged) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Execute the claimed fires that are due: pending ones, and running ones whose lease
    /// expired. A failure is retried with backoff; a routine that stopped being active has
    /// its fire cancelled.
    ///
    /// # Errors
    /// Returns a scheduler error when the ledger cannot be read or written.
    pub async fn execute_pending(
        &self,
        dispatch: &dyn RunDispatch,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<(usize, usize), SchedulerError> {
        let ledger = crate::control::routines::FireLedger::new(
            self.events.pool().clone(),
            self.tenant_id.clone(),
        );
        // Leased fires are independent: run them side by side (bounded), so a burst of routines
        // due at the same moment — every morning digest at nine — does not wait turn by turn.
        let outcomes: Vec<Result<Option<bool>, SchedulerError>> =
            futures_util::stream::iter(ledger.lease_due(now, limit).await?)
                .map(|fire| {
                    let ledger = &ledger;
                    async move { self.execute_fire(ledger, dispatch, fire, now).await }
                })
                .buffer_unordered(self.concurrent_fires)
                .collect()
                .await;
        let mut executed = 0usize;
        let mut failures = 0usize;
        for outcome in outcomes {
            match outcome? {
                Some(true) => executed += 1,
                Some(false) => failures += 1,
                None => {}
            }
        }
        Ok((executed, failures))
    }

    /// Execute one leased fire: `Some(true)` executed, `Some(false)` failed (retried later or
    /// failed for good), `None` cancelled because its routine is gone or no longer active.
    pub(crate) async fn execute_fire(
        &self,
        ledger: &crate::control::routines::FireLedger,
        dispatch: &dyn RunDispatch,
        fire: crate::control::routines::Fire,
        now: DateTime<Utc>,
    ) -> Result<Option<bool>, SchedulerError> {
        let Some(routine) = self.load(&fire.routine_id).await? else {
            ledger.cancel(&fire, "the routine no longer exists").await?;
            return Ok(None);
        };
        if routine.status != "active" {
            ledger
                .cancel(&fire, &format!("the routine is {}", routine.status))
                .await?;
            return Ok(None);
        }
        let window = fire.window_at.unwrap_or(now);
        match dispatch
            .fire_routine(self.fire_request(&routine, window, &fire.fire_key, fire.payload.clone()))
            .await
        {
            Ok(outcome) => {
                ledger
                    .complete(
                        &fire,
                        &routine.owner_user_id,
                        json!({
                            "objective_id": outcome.objective_id,
                            "run_id": outcome.run_id,
                            "window": fire.window_at.map(|at| at.to_rfc3339()),
                            "source": fire.source,
                        }),
                    )
                    .await?;
                Ok(Some(true))
            }
            Err(error) => {
                ledger
                    .fail(&fire, &routine.owner_user_id, &error.to_string(), now)
                    .await?;
                Ok(Some(false))
            }
        }
    }

    async fn load(&self, routine_id: &str) -> Result<Option<Routine>, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(
            "SELECT id, tenant_id, workspace_id, owner_user_id, teammate_id, trigger, \
             objective_template, absence_policy, status, last_fired_at, next_due_at \
             FROM routines WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(routine_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(routine_from_row).transpose()
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
    let factor: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(invalid("expected a suffix of s, m, h or d")),
    };
    let seconds = value
        .checked_mul(factor)
        .ok_or_else(|| invalid("interval is too long"))?;
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

    /// The first window strictly after `after` in `zone`. A wall-clock minute the zone skips
    /// stands for the first valid instant after its gap, and one it repeats for its first
    /// occurrence ([`ScheduleZone::resolve`]); the walk goes by wall-clock minutes, so a
    /// schedule that matches several minutes of one gap fires once, at the end of it.
    fn next_after(
        &self,
        after: DateTime<Utc>,
        zone: ScheduleZone,
    ) -> Result<DateTime<Utc>, SchedulerError> {
        let mut candidate = zone
            .local(after)
            .with_second(0)
            .and_then(|value| value.with_nanosecond(0))
            .unwrap_or_else(|| after.naive_utc())
            + Duration::minutes(1);
        let limit = candidate + Duration::days(366);
        while candidate <= limit {
            if self.matches(candidate) {
                if let Some(instant) = zone.resolve(candidate) {
                    if instant > after {
                        return Ok(instant);
                    }
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
            filters: Vec::new(),
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

    fn instant(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("instant")
            .with_timezone(&Utc)
    }

    /// Every due window of a cron schedule in `zone` between `from` and `until`, by repeated
    /// `next_due_after`, the way the scheduler walks it.
    fn windows(spec: &str, zone: &str, from: &str, until: &str) -> Vec<String> {
        let schedule = trigger(RoutineTriggerKind::Cron, spec, zone);
        let (mut cursor, until) = (instant(from), instant(until));
        let mut out = Vec::new();
        while let Some(next) = schedule.next_due_after(cursor).expect("next") {
            if next > until {
                break;
            }
            out.push(next.format("%Y-%m-%dT%H:%M:%SZ").to_string());
            cursor = next;
        }
        out
    }

    #[test]
    fn a_spring_forward_gap_fires_once_at_the_end_of_the_gap() {
        // New York, 2026-03-08: 02:30 does not exist; the daily 02:30 window fires at 03:00 EDT.
        assert_eq!(
            windows(
                "30 2 * * *",
                "America/New_York",
                "2026-03-06T12:00:00Z",
                "2026-03-10T12:00:00Z"
            ),
            [
                "2026-03-07T07:30:00Z",
                "2026-03-08T07:00:00Z",
                "2026-03-09T06:30:00Z",
                "2026-03-10T06:30:00Z"
            ]
        );
        // London, 2026-03-29: 01:30 does not exist; fires at 02:00 BST (01:00Z).
        assert_eq!(
            windows(
                "30 1 * * *",
                "Europe/London",
                "2026-03-27T12:00:00Z",
                "2026-03-31T12:00:00Z"
            ),
            [
                "2026-03-28T01:30:00Z",
                "2026-03-29T01:00:00Z",
                "2026-03-30T00:30:00Z",
                "2026-03-31T00:30:00Z"
            ]
        );
        // Auckland, 2026-09-27: 02:30 NZST does not exist; fires at 03:00 NZDT (14:00Z the day before).
        assert_eq!(
            windows(
                "30 2 * * *",
                "Pacific/Auckland",
                "2026-09-25T00:00:00Z",
                "2026-09-29T00:00:00Z"
            ),
            [
                "2026-09-25T14:30:00Z",
                "2026-09-26T14:00:00Z",
                "2026-09-27T13:30:00Z",
                "2026-09-28T13:30:00Z"
            ]
        );
        // Lord Howe, a thirty-minute shift: 02:15 does not exist on 2026-10-04; fires at 02:30 (15:30Z).
        assert_eq!(
            windows(
                "15 2 * * *",
                "Australia/Lord_Howe",
                "2026-10-02T00:00:00Z",
                "2026-10-06T00:00:00Z"
            ),
            [
                "2026-10-02T15:45:00Z",
                "2026-10-03T15:30:00Z",
                "2026-10-04T15:15:00Z",
                "2026-10-05T15:15:00Z"
            ]
        );
        // A schedule that matches several minutes of the gap still fires once.
        assert_eq!(
            windows(
                "*/15 2 * * *",
                "America/New_York",
                "2026-03-08T00:00:00Z",
                "2026-03-08T12:00:00Z"
            ),
            ["2026-03-08T07:00:00Z"]
        );
    }

    #[test]
    fn a_fall_back_repeat_fires_once_at_the_first_occurrence() {
        // New York, 2026-11-01: 01:30 happens at 05:30Z (EDT) and 06:30Z (EST); only the first is a window.
        assert_eq!(
            windows(
                "30 1 * * *",
                "America/New_York",
                "2026-10-30T12:00:00Z",
                "2026-11-03T12:00:00Z"
            ),
            [
                "2026-10-31T05:30:00Z",
                "2026-11-01T05:30:00Z",
                "2026-11-02T06:30:00Z",
                "2026-11-03T06:30:00Z"
            ]
        );
        // London, 2026-10-25: 01:30 happens at 00:30Z (BST) and 01:30Z (GMT).
        assert_eq!(
            windows(
                "30 1 * * *",
                "Europe/London",
                "2026-10-23T12:00:00Z",
                "2026-10-27T12:00:00Z"
            ),
            [
                "2026-10-24T00:30:00Z",
                "2026-10-25T00:30:00Z",
                "2026-10-26T01:30:00Z",
                "2026-10-27T01:30:00Z"
            ]
        );
        // After the first occurrence has fired, the second one an hour later is not found.
        let schedule = trigger(RoutineTriggerKind::Cron, "30 1 * * *", "America/New_York");
        assert_eq!(
            schedule
                .next_due_after(instant("2026-11-01T05:30:00Z"))
                .expect("next"),
            Some(instant("2026-11-02T06:30:00Z"))
        );
        // An unaffected hour is untouched on the same day.
        assert_eq!(
            windows(
                "0 9 * * *",
                "America/New_York",
                "2026-10-31T12:00:00Z",
                "2026-11-02T12:00:00Z"
            ),
            ["2026-10-31T13:00:00Z", "2026-11-01T14:00:00Z"]
        );
    }

    #[test]
    fn zones_without_daylight_saving_and_utc_do_not_shift() {
        assert_eq!(
            windows(
                "0 9 * * *",
                "Asia/Kolkata",
                "2026-03-06T00:00:00Z",
                "2026-03-09T00:00:00Z"
            ),
            [
                "2026-03-06T03:30:00Z",
                "2026-03-07T03:30:00Z",
                "2026-03-08T03:30:00Z"
            ]
        );
        assert_eq!(
            windows(
                "0 9 * * *",
                "America/Sao_Paulo",
                "2026-03-06T00:00:00Z",
                "2026-03-09T00:00:00Z"
            ),
            [
                "2026-03-06T12:00:00Z",
                "2026-03-07T12:00:00Z",
                "2026-03-08T12:00:00Z"
            ]
        );
        assert_eq!(
            windows(
                "0 9 * * *",
                "UTC",
                "2026-03-06T00:00:00Z",
                "2026-03-08T00:00:00Z"
            ),
            ["2026-03-06T09:00:00Z", "2026-03-07T09:00:00Z"]
        );
    }

    #[test]
    fn an_unknown_zone_fails_closed_when_the_trigger_is_validated() {
        let bad = trigger(RoutineTriggerKind::Cron, "0 9 * * *", "Mars/Olympus_Mons");
        assert!(matches!(
            bad.validate(),
            Err(SchedulerError::UnknownTimezone(_))
        ));
        assert!(matches!(
            bad.next_due_after(Utc::now()),
            Err(SchedulerError::UnknownTimezone(_))
        ));
        assert!(
            trigger(RoutineTriggerKind::Cron, "0 9 * * *", "Europe/Paris")
                .validate()
                .is_ok()
        );
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
