# APP-010 reconciliation (2026-09-23, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `control/routines` was `fire_key`/`should_fire`
(a string format and a `contains`), `notify` held no notification code, the desktop module
13 lines (docs/review/2026-09-18-m5-placeholder-audit.md). That branch is parked as
`stale/APP-010-routines-2026-09-15`.

- **Canonical owners.** `crates/server/src/control/routines/` — Routine definitions
  (`routines`) and the firing ledger; `crates/server/src/notify/` — notifications,
  preferences and channel delivery; `apps/desktop/src/automation/` — the client half.
  Timing stays CORE-008's (`scheduler/routine.rs`: cron/interval, `next_due_at`,
  absence policy); Run/Objective creation stays the runtime's and the graph's, reached
  through the same functions `PostMessage` uses.
- **Existing code, coverage PARTIAL, kept.** `scheduler::RoutineScheduler` (timing,
  absence policy, `routine.fired`/`routine.skipped`), `TimerStore`, `WaitRegistry`,
  `Scheduler::tick`, the `RunDispatch` port, the `routines`/`notifications`/
  `notification_preferences` tables, the `event_cursors` machinery (CORE-009).
- **Gaps found.**
  1. *Restart can duplicate a scheduled run.* `tick` calls `fire_routine` and only then,
     in a second transaction, advances `next_due_at`; `GraphDispatch::fire_routine`
     ignores its `fire_key`. A crash between the two fires the same window again.
  2. *A routine does not use the interactive path.* `GraphDispatch::fire_routine` creates
     a fresh unnamed AgentThread, never computes a capability projection and never runs
     a turn: the run sits in `RUNNING` doing nothing.
  3. *Nothing runs the scheduler.* The deployable (`composition.rs`) serves the API only.
  4. No routine commands (`CreateRoutine` … `TriggerRoutineNow`), no event triggers, no
     notifications, preferences, channels or retry.
- **Ownership decisions (recorded, not assumed).**
  - *A firing ledger makes a window fire once.* `routine_fires` (migration 0016) holds one
    row per `fire_key` (`routine:<id>:<window>` for a schedule, `routine:<id>:event:<event
    id>` for an event, `routine:<id>:manual:<command id>` for `TriggerRoutineNow`) under a
    unique index. Claiming a window inserts the row and advances `next_due_at` in one
    transaction with `routine.fired`; executing a claimed fire leases it (`FOR UPDATE SKIP
    LOCKED`), and every step of the execution is looked up before it is created (the
    routine's thread, the fire's message by its `fire_key`, the run by its trigger
    message), so a restart or a second scheduler instance resumes a fire instead of
    repeating it. A failed execution is retried with exponential backoff up to a bound,
    then marked `failed` (`routine.fire_failed`) and the owner is notified.
  - *Same path as interactive work.* The deployable's `RunDispatch` fires a routine
    exactly as `PostMessage` answers a message: the routine's objective thread gets the
    prompt as a message, `route_to_teammate` picks the routine's teammate (or the
    workspace default), the run's capability projection is seeded from the same policies,
    the turn runs through the same engine, tool calls go through the same policy and
    Effect Ledger, and the reply is posted to the thread. Interactive runs are never
    dispatched by the scheduler (their handler owns them).
  - *Event triggers are an event-log consumer.* `trigger {kind: event, spec}` matches an
    event type (`family.name` or `family.*`) in the routine's workspace; the consumer reads
    the tenant's RuntimeEvents after its `event_cursors` position and claims fires in the
    same transaction that advances the cursor, so a replayed or concurrently processed
    event claims one fire. Events the routine itself produced (its thread, its runs, the
    scheduler) never trigger it.
  - *Notifications are an event-log consumer too.* `notify::Notifier` turns mentions
    (`thread.attention_changed`), questions (`question.asked`), approval requests
    (`approval.requested`) and routine outcomes (`run.succeeded|failed|blocked` of routine
    runs, `routine.fire_failed`) into `notifications` rows — unique per (source event,
    user, kind) (migration 0016) — with the channels the user's preference for that
    workspace × kind selects (default in_app + desktop; email is opt-in), and emits a
    user-scoped `notification.created`. The stream delivers `notification.*` only to the
    user it names. Desktop native notifications are shown by the desktop main process from
    that event; email (OD-009) is sent server-side through an HTTP mail API configured by
    `QUANSIO_EMAIL_ENDPOINT` / `QUANSIO_EMAIL_TOKEN`, as a `notification_deliveries` row with
    retry and exponential backoff; without configuration the email channel is recorded
    `unavailable`, never silently dropped.
  - *The deployable runs one background loop* (`QUANSIO_SCHEDULER_INTERVAL_MS`, default
    1000 ms; `QUANSIO_SCHEDULER=off` disables it): per tenant, the CORE-008 tick, the
    routine event consumer, the pending-fire executor, the notifier and email delivery.
- **Contracts.** DOMAIN.md §13.1 (the firing ledger and its keys, retry), §13.3 (default
  channels, delivery rows, user-scoped events), §9.2 `routine.*` gains `updated`,
  `resumed`, `deleted`, `fire_failed`; `notification.*` gains `created`, `read`. Commands
  are the catalogued `CreateRoutine`, `UpdateRoutine`, `PauseRoutine`, `ResumeRoutine`,
  `TriggerRoutineNow`, `DeleteRoutine`, `UpdateNotificationPreferences`,
  `MarkNotificationRead`; projections `/v1/routines`, `/v1/notifications`.
- **Migrations.** `0016_routine_fires_notification_delivery.sql`: `routine_fires`,
  `notification_deliveries`, `notifications.source_event_id` with its unique index.
  Additive.
- **External effects.** Email delivery to the configured provider (tier 1 notification;
  no user content beyond the notification title). No real boundary is required
  (`real_boundary: false`); the E2E uses a local HTTP capture as the mail API.
