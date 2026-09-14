# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## acceptance

**Path traversal and unauthorized roots are denied.** Every file operation takes a `SandboxPath`, which
only `Sandbox::resolve` produces, so "was the path checked?" is not a question a call site can get
wrong. Three rules, and they are separate because they fail for different reasons: an operation naming a
root this sandbox was not given is refused *before* any path is considered (reaching the host filesystem
is a decision the runtime made, not a fallback a tool picked); a `..` component is refused outright,
whether or not the result would still land inside the root; an absolute path is refused rather than
reinterpreted. The rule that matters in practice is resolution: a symbolic link inside the root pointing
outside it is *textually* inside, so the test drives it and asserts the file outside was not written.

**Terminal replay resumes from the durable cursor without duplicating the command.** Two halves, and
they are tested separately because they are separate authorities. The worker's half
(`crates/qworkerd/src/tools/terminal.rs`) is the pure decision: an attach naming the session's
`last_command_id` is a `Replay` from the cursor, anything else is a `Dispatch`. The control plane's half
(`crates/machine/src/control/terminal.rs`) is the durable record: `last_command_id` is the record of what
ran, `attach` takes the decision and writes it under a row lock so two reconnecting workers cannot both
be told to run the same command, and the cursor only moves forward so a client cannot make a session
re-send bytes it has already consumed.

## the three named tests

- **path traversal** — `a_path_that_leaves_its_root_is_refused_before_any_io` and
  `a_root_must_be_absolute_existing_and_a_directory`.
- **PTY reconnect** — `a_reconnect_replays_from_the_durable_cursor_instead_of_running_the_command_again`
  (durable half) and `a_reconnect_resumes_from_the_durable_cursor_without_running_the_command_again`
  (worker half).
- **command cancellation** — `a_command_is_cancelled_while_it_runs_and_stopped_when_it_is`.

## the defect this task spent most of its time on

`TerminalStore::attach` was reported as hanging in the previous turn, with the test kept out of the tree
so it could not hang a pipeline. **That diagnosis was wrong, and the store was correct all along.** The
hang was in the shared test harness.

A probe replicating the failing test step by step (`diagnosis-probe.log`) reached
`M21: drop_pool` having completed every store operation, including all four attaches, with correct
results. `common::drop_pool` called `pool.close()` before dropping the scratch database, and
`PgPool::close` waits for every checked-out connection to be returned — a test calling it is usually
still holding one, because the connection is a local that outlives its last query. So the test waited
forever at teardown, which looked exactly like a hanging query.

Five other crates' harnesses had the same `pool.close().await`: `capability`, `events`, `graph`,
`indexer` and `server`. All six now drop the database with `DROP DATABASE ... WITH (FORCE)`, which
terminates whatever sessions are still attached, and leave the pool to be closed when its owner drops it.

**Measured effect.** Before this change the workspace suite was clean in 8 of 10 runs at `dd5e024` and 9
of 10 at `fb50673`, with failures in the runtime orchestration and turn-loop suites that I had recorded
as load-sensitive product flakes. After it: **7 of 7 clean, 496 tests across 80 suites, zero failures**,
with `workspace-tests.log` and `workspace-tests-run-2.log` as two of them. I am recording the measurement,
not claiming the earlier flakes were the same defect: the cancel over-report was a genuine product bug
fixed separately at `fb50673`, and the turn-loop `protocol_states_pkey` failure was never diagnosed.

Two further defects were found and fixed while building the worker's tool host, both caught by tests
rather than by review:

- a spawned process was never reaped, so it became a zombie whose pid still answered `kill -0` and a
  handle could not tell a running process from a finished one. `ProcessHandle` now owns its child and
  `cancel` signals *and* waits.
- after a kill, descendants of the command still held the pipe's write end, so waiting for EOF blocked
  for as long as the orphan lived — the cancellation test took 60 s instead of 0.37 s. The output of a
  stopped command is not what the caller asked for, so the readers are abandoned instead.

## recorded_decisions

- **The task splits across two crates, and not by preference.** The architecture gate forbids
  `crates/qworkerd` a Postgres client at all, because a worker reaches the server only over its fenced
  channel. EXEC-006's declared path is `crates/qworkerd/tools/`, so the durable session row cannot live
  there; it is owned by `crates/machine/src/control/`, the execution-model owner that already owns the
  target and lease tables. The worker keeps the conduit and the replay decision.
- **The terminal-session prefix was undeclared, and now is not.** `terminal_sessions.id` has carried
  `CHECK (id LIKE 'tsn_%')` since 0001 and DOMAIN 8.5 defines the entity, but 1.1's canonical id table
  never listed it — so the prefix existed in the schema and in no authority the identity catalog is
  generated from. `TerminalSession tsn_` is now in 1.1, `ids.yaml`, the identity proto enum and the core
  prefix table, additively within v1.
- **The conduit is pipe-backed with the PTY behind a trait.** The acceptance statement is about the
  durable cursor and single dispatch, which the trait's contract carries; PTY allocation is a substrate
  concern that would need a new dependency. Recorded as a limitation rather than approximated: a target
  that provides a PTY implements `TerminalConduit` and the replay rule is unchanged.
- **Children of a command are not necessarily stopped with it.** Killing the direct child does not stop a
  grandchild a shell left behind. Containing those is the execution target's job — a process group or
  cgroup around the target — and `cancel` documents that it does not claim it.
- **A declared content digest is checked before anything is written**, which is what makes `fs.write`
  idempotent in the Effect Ledger's sense: a retry claiming different bytes is a different call, not a
  replay. A patch is all-or-nothing for the same reason — a partially applied patch leaves the caller
  unable to tell what the file now is.

## what is not claimed

- The task's `real_boundary` is `false`: no PTY is allocated and no host network boundary is crossed.
- `crates/qworkerd`'s tool host is exercised over a real filesystem and real child processes, but its
  tools are not yet reachable through the full runtime dispatch path (that is RUN-011's turn loop, which
  is `PASS` at the stub-model level). What is verified here is the host's own contract.
