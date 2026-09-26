# CORE-008 — an on-time routine window fires whatever the absence policy

Found by QA-009 (P1) while measuring scheduling latency against the served release build: a routine
created with the default absence policy (`skip`, also the desktop's default) never fired on schedule.
`RoutineScheduler::tick` applied the absence policy to every due window, and `skip` drops a window —
so each window was recorded as `routine.skipped` ("missed_window") and the next one moved on.
`served-routine-before.log`: a 5 s routine on the release build of `main` (dc84014) — 0 fires, 4 skips
in 22 s. The existing tests covered a window 3 minutes late (correctly missed) and the served routine
tests used `catch_up_once`, so nothing exercised an on-time window under `skip`.

Fix (`crates/server/src/scheduler/`): the latest due window, reached within the misfire threshold
(`DEFAULT_MISFIRE_THRESHOLD_SECONDS` = 60, `SchedulerConfig::misfire_threshold`) with no later window
also due, is on time and fires under every policy; the policy decides only what a missed window means.
DOMAIN.md §13.1 now defines the on-time window.

Tests: `an_on_time_window_fires_under_every_absence_policy` (skip, queue and catch_up_once all fire a
just-due window once and advance), `the_misfire_threshold_decides_when_a_window_was_missed` (with no
tolerance the same window is skipped), the existing late-window test unchanged; the scheduler,
routines and graph-dispatch suites (`scheduler-tests.log`), scheduler unit tests, fmt and clippy.
Served (`served-routine.log`): the same 5 s routine on the fixed release build fires every window
(4 of 4, 0 skipped). Mutation: disabling the on-time branch fails the new test (`mutation.log`).
