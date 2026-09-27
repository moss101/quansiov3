# QA-001 evidence — unit, schema and contract verification

## What the task adds

| Piece | Where | What it holds the tree to |
|---|---|---|
| Test audit | `scripts/ci/test_audit.py`, last gate of `scripts/ci/ci_summary.py` | Reads the pipeline's own test logs: pytest (run with `-rsxX`), libtest, vitest (each package's JSON report, written when `QUANSIO_CI_LOG_DIR` is set), XCTest. Fails on a skip that names neither `BLOCKED_EXTERNAL` nor a `QUANSIO_TEST_*` variable, any xfail/xpass, any ignored Rust test, any vitest todo or unexplained skip, a missing test log; publishes the inventory of what did not run and why (`test-audit.json`). |
| Static bans | `tests/contract/test_qa001_gate.py` | `#[ignore]` anywhere in `crates/` (inline test modules too); TypeScript `.only`, `.todo`, `.fails`, and `.skip`/`.skipIf` outside a file that names its `BLOCKED_EXTERNAL` block; the existing pytest skip/xfail scan. |
| Coverage against DOSSIER §21.4 | `scripts/ci/coverage.py`, `scripts/ci/pycoverage.py`, `scripts/ci/coverage-expectations.json`, `coverage` job in `.github/workflows/ci.yml` | Rust: the workspace's tests built with `-C instrument-coverage` in their own target directory, continuous-mode profiles (a killed process still counts), exported with the `llvm-tools` matching the compiler's LLVM; lines held to the floors from a line-only run, branches reported from a `--branch` run. Python: the `python/` suite under a standard-library `sys.monitoring` tracer. `check` fails under a floor or on a missing measurement. |
| Tool tests | `tests/ci/test_qa001_tools.py` | Each tool on fixtures shaped like the real runners' output, with every failure path. |

Contract drift (`gen_contracts.py --check`) and contract compatibility (`contract_compat.py`) were
already gates; the schema suite validates the repository's real artifacts.

## Results

**Clean checkout (acceptance 1).** `git clone` of the branch at edf72d2 into an empty directory, no
`.env`, its own fresh `target/`, then `bash scripts/ci/ci.sh` (`clean-ci.log`, `clean-ci-summary.json`):
12 gates PASS, database-backed tests on (the dev stack's PostgreSQL 17). 374 crates compiled, 138 Rust
test binaries, 628 Python intelligence tests, 181 TypeScript tests (12 more blocked), 16 XCTest tests (9 blocked), 356 repository
tests. The test audit (`test-audit.json`) found no unexplained skip; what did not run is inventoried by
the variable it waits for (live model providers, macOS computer use and VM, the performance campaign).

**Contract drift (acceptance 2).** The `contract-drift` gate regenerates every contract
(catalog, OpenAPI, Rust, Python, TypeScript) and diffs: CLEAN. `contract-lint-compat` passes.

**Coverage against DOSSIER §21.4** (`coverage-check.log`, `report-final.json`; lines from the
line-only run `rust-lines-2.json`, branches from the `--branch` run `rust-branches-2.json`):

| Group | Lines | Floor | Branches |
|---|---|---|---|
| capability | 96.19 % | 90 % | 91.67 % |
| policy | 92.15 % | 90 % | 75.71 % |
| effects | 90.13 % | 90 % | 78.75 % |
| runtime | 96.05 % | 90 % | 87.50 % |
| recovery | 94.60 % | 90 % | 85.71 % |
| secrets | 92.75 % | 90 % | 83.93 % |
| egress | 94.95 % | 90 % | 81.54 % |
| qworkerd protocol | 90.00 % | 90 % | 90.91 % |
| other Rust | 86.34 % | 75 % | — |
| Python service modules | 87.50 % | 80 % | — |
| TypeScript | not measured | 70 % | `BLOCKED_EXTERNAL` (no approved coverage provider) |

Before this task's tests the same measurement read capability 77.16 %, effects 85.85 %, runtime
85.62 % and recovery 70.69 % (`report.json`). The instrumented runs passed every test binary (the
line run: 138 of 138).

## Tooling limits (recorded, not hidden)

- `@vitest/coverage-v8` and coverage.py were not approved for download (2026-09-27): TypeScript
  coverage is `BLOCKED_EXTERNAL`; Python uses the stdlib tracer. The `llvm-tools` rustup component
  was approved and installed.
- Rust branch regions need rustc's unstable `-Z coverage-options=branch` (enabled with
  `RUSTC_BOOTSTRAP=1` for the measurement build only); `llvm-cov` 22.1.8 segfaults exporting 13 of
  the workspace's files built that way, whatever the export flags. Lines are therefore held on a
  line-only run, which exports every file; branch counts are reported beside them from files the
  branch run could export.
- macOS continuous mode needs the counter, data and bitmap sections page-aligned at link time
  (`-Wl,-sectalign,__DATA,<section>,0x4000`), or the runtime writes nothing.
- A Rust test that returns early with a `BLOCKED_EXTERNAL` marker passes in libtest; the marker goes
  to captured stderr, so the audit cannot inventory those (the ban on `#[ignore]` is enforced).
- Commit f4834d5's message says served policy filtering "read 0%": that reading came from a
  hand-picked object list that left out the test binary that runs it; the full export counts it.

## Findings

| Id | Finding | Where fixed |
|---|---|---|
| C-1 | `Recoverer` was never composed into the served runtime: a restart honoured no cancellation requested before the stop and reported no uncertain effect ("killed runtime resumes to the same safe logical point" held only for the library). | RUN-009 (merge 99f5893): `serve()` recovers every tenant's non-terminal runs first; served check in `cli_e2e`; mutant that skips the pass killed |
| C-2 | Capability user rules (`always` refused at tier 4, where policy does not allow, over `never`; `never`; `ask`; expiry) and policy deny/ask/tier ceilings never ran through `assemble`; selector containment untested for six of nine kinds; `authorize`'s expired-grant and tier-ceiling denials unexercised. | tests in `crates/capability/tests/` |
| C-3 | The effect taxonomy's fail-closed validation (version, empty, unknown/missing/duplicate class, missing decision, bad tier/strategy, unreadable file) and a reservation whose tier disagrees with the taxonomy were untested. | `crates/server/tests/effects_taxonomy_and_model.rs`, `effects.rs` |
| C-4 | Recovery's unknown-run refusals, the recovery port, fence events and the reconciliation predicate were untested. | `crates/server/tests/recovery.rs` |
| C-5 | A tool call refused after its effect was marked DISPATCHED (every host refusal or unavailability, every internal-tool refusal) tried to cancel it, which the ledger refuses from DISPATCHED: the turn failed, the call stayed `dispatched`, and recovery demanded reconciliation for a call that never ran. | RUN-011 (merge af30017): settles as a known failure; both paths' tests fail on the old code |
| C-6 | An inline-joined delegation wrote the child's result into the attempt's `error` column; a round that ran out of step budget left recorded calls dispatched but never sent. | RUN-001 (merge e0be8c1) |
| C-7 | Question expiry had no caller and `answer()` ignored `expires_at`: questions never expired and a WAITING_QUESTION run waited forever. | RUN-011 (merge 14e07a7): the background pass expires due questions; a late answer is refused |
| C-8 | Model-driven plans, memory proposals and completion claims are not composed in the served product: `work.propose_plan`/`memory.propose` reach Unavailable ports, no tool or context gives a model the WorkGraph revision a plan needs, nothing produces a completion claim, `ContractVerifier` is never installed, the Python semantic verifier has no RPC, and the intelligence plane has no durable memory sink. | open — design work handed to two follow-up tasks (owners RUN-008, RUN-011, RUN-003, INT-007, INT-005) |
| C-9 | `work.delegate`'s `work_node_id` is optional in its schema but a required digest argument; plan GraphErrors lose their type and audit event (`graph_port.rs:304`); suspected `parent_id`/`parent_of` cycle-direction inconsistency. | open — in the plans follow-up task |

## Coverage tests added

Capability (34), recovery (2), effects (7), and — written by two delegated agents in their own
worktrees, reviewed and run here — 82 runtime tests: internal control tools, approval resume edges,
questions, plan compile and graph port, verification contracts and checks, compaction, AgentGraph
store, run store, engine, orchestration and budgets (`crates/server/tests/turn_loop_internal_tools.rs`,
`questions_lifecycle.rs`, `planning_compile.rs`, `planning_graph_port.rs`,
`verification_contract_checks.rs`, `compaction_unowned_epochs.rs`, `agent_graph_store.rs`,
`state_machine_store.rs`, `state_machine_engine.rs`, `orchestration_paths.rs`,
`budget_service_paths.rs`). Lines they could not cover honestly are unreachable (recorded by file and
line in the agents' reports: e.g. `orchestration/service.rs:206-210`, `state_machine/store.rs:1013-1019`,
`dispatch.rs:1870-1878`), races between an unlocked read and a locked re-read, or database-error
mapping.
