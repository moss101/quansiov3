# APP-010 — routines, schedules, events and notifications

## What was built (task branch `task/APP-010-routines`)

The 2026-09-15 record was a placeholder (`fire_key`/`should_fire`, no notifications).
Ownership decisions are in `evidence/APP-010/RECONCILIATION.md`. Found on the way: a
restart between `fire_routine` and the `next_due_at` update fired a window twice; a
routine's run skipped the interactive path (fresh unnamed AgentThread, no capability
projection, no turn); and the deployable never ran the scheduler.

### Server

- `control/routines/` (the task's path): `RoutineStore` (create / update / pause / resume /
  delete / trigger-now, each with its `routine.*` event; trigger, template and teammate
  validated), the firing ledger (`routine_fires`, migration 0016: one row per fire key;
  claimed with `next_due_at` in one transaction; leased execution; retry with backoff up to
  5 attempts, then `routine.fire_failed`; cancel when the routine stopped), and event
  triggers (a RuntimeEvent consumer that claims a fire per matching event in the
  transaction that advances its `event_cursors` position; a routine's own events, runs,
  thread and objectives never trigger it).
- CORE-008's `RoutineScheduler::tick` now claims a window in the ledger (guarded by the
  window it read) and executes leased fires (`execute_pending`); its absence-policy tests
  keep passing unchanged.
- `notify/` (the task's path): notifications and per user × workspace × kind preferences
  (default in-app + desktop; empty mutes; email opt-in), the notifier consumer (mentions,
  questions, approval requests, routine outcomes — once per event, user and kind), and
  server-side email (OD-009) through an HTTP mail API with retry and backoff, `unavailable`
  when no transport is configured.
- API (`api/routines.rs`): the eight catalogued commands and `GET /v1/routines`,
  `GET /v1/notifications`. `InteractiveDispatch` executes a fire by `PostMessage`'s steps —
  the routine's objective thread gets the prompt as a message, `route_to_teammate`, the
  seeded capability projection, the engine's turn, the posted reply — looking each step up
  first, so a resumed fire continues. Runs a person or a routine started are never
  dispatched by the scheduler.
- The deployable runs one background loop (`composition::Background`, default every
  second; `QUANSIO_SCHEDULER=off`) per tenant over `background_tenants()` (security
  definer, ids only): routine triggers, the CORE-008 tick, the notifier, email.
- The stream delivers `notification.*` only to the user it names.
- DOMAIN.md §13.1 (ledger, keys, execution path, event triggers, pause), §13.3 (channels,
  defaults, notifier, delivery), §9.2 (`routine.*`, `notification.*` names).

### Desktop (`apps/desktop/src/automation/`)

`routines.ts` (decode, draft checks, commands, the controls a state allows, a fire summary
that makes any duplicate window or run visible) and `notifications.ts` (decode, in-app
list, preference toggles, user-scoped events, the native-notification decision). Main:
`routines.list/command`, `notifications.list/read/preference`, and an Electron
`Notification` for `notification.created` with `desktop: true`. The Automations page.

## Acceptance → tests

- *Restart does not duplicate scheduled runs.* `routines.rs`
  `a_scheduled_window_fires_once_across_restarts`: a window fires once; a second instance
  finds nothing; a crash before execution is retried after backoff and fires once; a crash
  after the run existed is resumed and reuses the run; an expired lease runs once; two
  racing instances claim one window — five windows, five fires, five runs.
  `automation.e2e.test.ts` kills the server process mid-flight, starts a new one on the same
  database, and checks that no window and no run appears twice among the fires.
- *Routine uses the same policy/capability/effect path as interactive work.* The same test
  reads the fire's run projection (`capability_projection.grants`), the prompt message by
  `routine:<id>` and the teammate's reply posted by the run — the steps of `PostMessage`.
- Required tests: schedule restart test (above), event dedupe test
  (`an_event_fires_its_routine_once_however_it_is_consumed`: one fire per event across a
  second scan, a replay from a lost cursor and two concurrent consumers; the routine's own
  activity does not trigger it), notification preference E2E (`automation.e2e.test.ts`:
  in-app only → no native notification, no mail; in-app + desktop + email → a native
  notification and an email with the bearer token through a local mail-API capture; the
  owner receives none of the editor's notification events), plus
  `notifications_follow_preferences_and_deliver_email` (retry/backoff, `unavailable`,
  mute, dedupe on re-consumption, read, routine outcome notifications).

## Full CI

`bash scripts/ci/ci.sh` on this branch (after merging main): 11/11 gates PASS
(`ci-summary.json`). Two earlier runs failed only on
`tests/integration/test_dev_stack.py::test_reset_then_reseed_is_deterministic`, a Docker
Desktop init-script race in the dev stack unrelated to this change; it was fixed and
recorded as a GOV-007 regression (`evidence/GOV-007/2026-09-23T07-04-49Z/`) and merged to
main before this run.
