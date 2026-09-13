# Evidence notes — INT-008 reconciliation

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## What this bundle records

The reconciliation for INT-008 and the environment fact that changes what is verifiable: the host
incident that had been blocking execution of newly created binaries for this whole session has
**cleared**, so `cargo test`, `scripts/dev/bootstrap.sh` and the full `bash scripts/ci/ci.sh` pipeline
are runnable again.

## The baseline pipeline is green

`ci-pipeline.log` is `bash scripts/ci/ci.sh` (with `QUANSIO_TEST_POSTGRES_URL` set): **11 of 11 gates
PASS**, `pipeline: PASS`. This is the first complete pipeline run since the incident began, and it
covers the gates that had been unrunnable all session, including `contract-drift` (which executes the
freshly built Rust contract generator) and `toolchains` (which compiles and runs the whole workspace
test set: 73 suites, plus pnpm, swift and the Python plane).

## The one transient failure, classified rather than hidden

`ci-first-run-transient.log` is the first attempt: `toolchains` FAILED (exit 101, `tests/turn_loop.rs`,
after a full workspace compile). It was classified as **transient, not a regression**, on evidence
rather than assumption:

* the same suite run standalone passes 12/12 (`cargo test -p quansio-server --test turn_loop`);
* the same gate re-run on the same tree passes with `bootstrap: OK` and 73 suites ok
  (`toolchains-gate.log`);
* the full pipeline then reports `pipeline: PASS`.

So the failure is recorded as flaky/contention (it happened while the workspace was compiling from an
emptied `target/`, on a machine shared with other agents), **not** as a product failure and **not** as a
pass: the green run is what is claimed, and this file is why the red one is not.

## What the reconciliation decided (see RECONCILIATION.md)

The Rust half owns the epoch lifecycle because an epoch is runtime state; the Python half owns what a
summary may contain and how a bounded conversation projection is assembled. INT-008's acceptance
statements are therefore enforced in two places: installation must refuse history a fork or revert
abandoned (inside the installing transaction, recording `rejected_stale`), and protocol replay must not
read `compaction_epochs` at all — which is checkable against RUN-009's declared recovery read set.
