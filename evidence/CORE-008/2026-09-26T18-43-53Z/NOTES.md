# CORE-008 — due work is dispatched when it falls due

Found by QA-009 against DOSSIER §21.2 ("runtime scheduling latency, ready → dispatched ≤ 500 ms p95"),
measured from outside on the release build: twenty 5 s routines at shipped defaults.

| Build | window → run start p50 / p95 |
|---|---|
| main with the on-time fix (one-second loop, fires executed inside the pass) | 2.3 s / 3.4 s |
| + fires executed side by side inside the pass | 1.5 s / 1.8–7.9 s (host load 9–11) |
| + wake at the next due window, fires run by an executor beside the pass | 0.26–0.33 s / 0.59–0.70 s (host load 6–9) |

Three causes, three changes:

1. **A fixed one-second loop.** A window waited for the next pass. The loop now sleeps until the
   earliest due routine window or timer across tenants — `background_next_due()` (migration 0024, a
   security-definer function that returns one timestamp, like `background_tenants()`), at least 50 ms
   and at most the interval (`composition::wait_until`). Work that fell due during a pass runs at once;
   work already due when the pass began (a backlog beyond one pass's bounds, or an item the pass could
   not clear) waits the whole interval, so nothing can spin the loop.
2. **Fires executed inside the pass.** A pass ran each claimed fire's whole turn, one after another, so
   every window falling due meanwhile waited to be claimed (a second from window to claim). A pass now
   only claims (`SchedulerConfig::execute_routine_fires = false` in the served loop) and wakes a
   `FireExecutor` (`scheduler/executor.rs`) as soon as its tenant's tick claims fires; the executor
   leases them and runs each as its own task, at most 16 per process. The fire lease still keeps one
   fire to one execution, and an expired lease makes a fire due again. `TriggerRoutineNow` and a
   scheduler without an executor keep executing inline — now up to eight fires side by side.
3. **The loop's wake-up counted claimed fires** (pending until the executor leases them) as due work,
   so it waited its whole interval: the wake-up counts only routine windows and timers.

What remains from claim to run start (p50 ≈ 150 ms) is the fire's own sequential work — objective,
thread, teammate routing, message, run — on a host shared with other work; QA-009 records the served
numbers with the host's load.

Tests (`scheduler-tests.log`, `unit.log`): `routines_due_together_fire_side_by_side` (eight 300 ms fires
in well under the 2.4 s a sequence takes), `a_pass_only_claims_and_the_executor_runs_the_fires_beside_it`
(the pass dispatches nothing; the executor starts all eight side by side; a second drain starts none),
`the_background_loop_learns_when_the_next_work_falls_due`, and the three `wait_until` unit tests; the
routines, graph-dispatch and schema-bootstrap suites; fmt and clippy (`lint.log`, statuses captured
before filtering). Mutations (`mutation.log`): work that fell due during a pass waits the interval; the
executor runs one fire at a time; the on-time window is not fired — each after a passing control.
