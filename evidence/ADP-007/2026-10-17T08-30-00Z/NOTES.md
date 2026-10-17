# ADP-007 regression: desktop E2Es after the plan limits

Found while verifying other work; neither failure was caused by it.

1. `automation.e2e.test.ts` created a routine with a 1 s interval and got AUTOMATION_LIMIT_EXCEEDED once ADP-007 added
   `min_interval_seconds` (dfc4a13e). The shipped file is compiled in and `schemas/json/automation-config.schema.json`
   floors the interval at 10 s, so even `tenant_overrides` cannot allow 1 s. Fix: the server reads
   `QUANSIO_AUTOMATION_CONFIG` (same shape; `AutomationConfig::from_env`, held to the schema's bounds in code); the E2E gives
   its scratch tenant a 10 s override and the routine uses `10s`. The product limit is unchanged and the test still asserts a
   routine keeps firing across SIGKILL and relaunch with no duplicate window or run.
2. `work.e2e.test.ts` failed under load with `CONFLICT_REVISION: base revision N is not the current graph revision N+1`, and in
   another run with a rebase that dropped `gather -> ready`. Not a graph race: the background orchestrator
   (`composition.rs`, one tick per workspace with graph work) promotes a node with no open prerequisite on its own, so any base
   revision can go stale between a load and a propose. That is what CONFLICT_REVISION and rebase are for. The test's
   non-conflict submits now reload, rebase and retry (refused proposals write nothing); the deliberate conflict stays strict
   and the rebase may drop only the now-redundant "already ready" transition.

Not fixed, not caused by this change (same failures on the primary checkout's older server binary):
`conversation.e2e.test.ts` (fs.read `rejected`) and `trust.e2e.test.ts` (effect never settles). Since EXEC-002 `fs.*` is
refused WORKER_UNAVAILABLE unless a worker attaches (`composition.rs`), while those tests expect the conformance stub to
answer. `local-files-watcher.test.ts` also failed in 2 of 6 full runs (OS watcher under load).

Stability: work + automation passed in 3 of 3 paired runs and in 6 of 6 full-suite runs after the final edit (the full suite still exits 1 only on the unrelated failures above).
