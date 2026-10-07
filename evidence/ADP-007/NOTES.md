# ADP-007 notes

What ran for real, what did not, what was deferred, and the traps worth keeping.

## What ran for real

- **PostgreSQL.** Every `automation_*` suite runs against a private scratch database it creates and drops (migration 0028 applied
  by the suite). The hook path and the script gate path also run as the least-privileged role `quansio_app` (row-level security
  applies; the endpoint lookup is a SECURITY DEFINER function because a request has no tenant).
- **HTTP.** `automation_triggers` serves the router on a loopback listener and posts signed requests to `/v1/hooks/:endpoint`
  (including a raw chunked POST to prove the 64 KiB cap holds without a Content-Length).
- **The scheduler.** The DST test drives `RoutineScheduler` windows across real transitions (America/New_York, Europe/London,
  Pacific/Auckland, Australia/Lord_Howe, Asia/Kolkata, America/Sao_Paulo) and asserts the fire ledger.
- **The served process, killed and relaunched.** `automation_served` spawns the real `quansio-server` binary (SIGKILL, then a new
  process over the same database) with the real Python intelligence plane as its own process and a loopback OpenAI-compatible
  provider that the test hosts. Two scenarios: a hook delivery accepted and its fire claimed while the background loop is off, the
  process killed, a relaunched process executing the fire exactly once; and a hook-fired workflow script killed while one worker
  is waiting on the model, relaunched, finished from its journal with no finished call asked of the model again.
- **The real intelligence plane.** `automation_untrusted` builds the turn's context through the Python plane (`BuildContext`) and
  calls the dev stub provider through the gateway: the payload is its own segment labelled `untrusted_external`, rendered inside
  the typed boundary, and a model that obeys the payload is parked on an approval (the model's obedience is simulated by a wrapper
  that adds the injected `fs.write`; everything after it is the product).
- **The approval path.** Script gates are `record.create` effect records on `{kind: work, selector: workflow:<run>}`, parked with
  `ApprovalRuntime::park_for_approval`, approved or denied through `ApproveEffect`/`DenyEffect`, and consumed with
  `verify_and_consume_receipt`; a denial ends the run.
- **Desktop.** `vitest` over the new helpers and static component renders; `tsc` and `eslint --max-warnings 0` clean.

## Not exercised live

- **A real model.** Every model in these tests is a stub or a scripted stand-in. Nothing here is real-model evidence (`real_boundary`
  is false for the task). The worker reply format (one JSON value, optionally in one fence) is exercised only against stubs.
- **A browser or Electron for the new Automations UI.** The component tests are static renders (no jsdom in the repo); clicks, focus
  moves and the clipboard are covered only through the pure helpers and by reading the code. The existing desktop e2e suites for
  automation skip without their real-boundary variables.
- **Time zones outside the six tested.** The rules are chrono-tz's database (0.10.4, IANA 2025); a database update changes
  transitions the way an operating system update would.
- **A second server instance racing the first.** The advisory lock per script run is exercised in-process (a transaction holds
  the run's lock; a second executor reports `Busy` and makes no call; released, the run finishes with each call asked once), not
  with two served processes.
- **`test_generated_bindings_are_current`** cannot run from a worktree (the contract generator wants the primary workspace); it runs
  in the finalize dry-run clone.

## Deferrals (recorded, not hidden)

- **Connector-event triggers.** No connector event source exists in the repository (no connector emits events into the event log),
  so the `event` trigger stays an inert subscription to `connector.*` families: it matches nothing until an event source exists. No
  code path was added for it. (DOMAIN.md §13.8 A.)
- **Authoring a workflow script in the desktop form.** The Automations form carries a stored script through an edit (a full
  replacement would otherwise drop it) and shows its graph and budget, but does not author or edit one; scripts are created through
  `CreateRoutine`/`UpdateRoutine` (the API and SDK).
- **Per-item status in the script view.** The projection gives counts per parallel step (total, finished, failed), so the view shows
  counts and a progress bar, not a list of individual items.
- **Retrying a failed worker call.** A worker whose run fails is journalled as that call's failure and counted once; there is no
  automatic retry (no blind retry of anything that may have had an effect). A call that was merely in flight when the process died
  is asked of the model once more: a model call is a read, not an external effect.
- **Reference extractions.** The reference extraction files named in the brief were not present in the checkout; the design was
  taken from the brief and the repository's own documents.

## Traps worth recording

- **An orphaned `active` turn stalls forever.** `RuntimeEngine::recover` only classifies; for a run that can continue it returns
  `Continue` and leaves the turn `active`, so a port that merely calls it and waits never makes progress. The served kill test found
  this (the in-process test could not: it simulates the death after the turn finished). The port now fails the in-flight model step,
  aborts the turn and runs a new one, unless recovery reports an effect to reconcile or a wait to honour.
- **`wrap_stream` is unavailable in this reqwest build.** A chunked body in a test is a raw TCP write.
- **A command id is idempotent over its parameters.** Reusing id 50 with other params is an idempotency mismatch, not a new
  command; tests allocate ids.
- **The planner cannot connect nodes created in the same transaction.** Script nodes land in one proposal and their edges in a
  second.
- **A node's CompletionContract that binds nothing.** A worker is not offered `work.complete`; its reply is the result and the
  runtime verifies it against the step's schema, so `done` comes only through `VerificationPassed`.
- **Workers authored `routine:<id>` count as the routine's runs.** A script run is one script run plus one worker run per call;
  tests that count "runs of a routine" must say which.
- **Hand-written ids must be 26 Crockford characters** (a stray `U` or a 27-character id fails the command, not the CHECK).
- **The format hook rewrites files between a Read and an Edit**; edits to files the hook touches were made with patch scripts.
- **chrono-tz** is a new dependency (with phf and siphasher): it passed DOSSIER §18, `deny.toml` and the supply-chain gate.

## Mutation checks

`mutation-checks.py` (this bundle) breaks 39 guards on purpose (replay ledger, signature and staleness, paused hook, delivery id,
the three layers of the body cap, rate limits, revision check and advance, idempotent create, plan limits, time zone rules,
journal replay of single steps and items, call start, call budget, approval threshold, schema validation of results, script
structure, recovery of an orphaned turn, the attention item, the untrusted block, its label, its segment and its boundary markers,
four desktop guards). A mutant counts only when it compiles, its control passed and a named test then fails.

- The first full run found two holes in the tests, one mutant that was too narrow and one robustness gap in the code, each closed and re-run: the body cap has
  three layers, so the mutant now removes all three (a single layer survives by design); a finished single step called again
  after a restart left the model untouched (the port is idempotent) but wrote a second `call_finished`, which the restart test now
  asserts cannot happen; two mutants made a fan-out spin forever, which the suite could only report as a timeout, so a fan-out
  that makes no progress over `items + 4` rounds now ends the step with a typed `STALLED` failure and the tests bound every
  pass in time; the call-cap refusal now has to name the guaranteed calls.
- **One mutant survives, by layering, and is left standing:** removing `room` in the fan-out loop (items start without
  regard to the cap). The cap is enforced before any item starts, at the fan-out gate (`capacity(used + remaining + items)`), so
  `room` is a second layer that no reachable journal can exercise; a test that reached it would have to fabricate a journal the
  executor never writes.
