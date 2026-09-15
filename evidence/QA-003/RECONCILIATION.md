# QA-003 reconciliation

- Canonical owner: `tests/recovery/` (task paths) with the durable matrix in
  `crates/server/tests/qualification.rs` — Rust must live under `crates/`
  (workspace_check), so the root suite is the standing gate and the Rust suite
  drives the real runtime against PostgreSQL.
- Existing: RUN-009 recovery/fencing (`runtime/recovery`), RUN-010 budgets,
  CORE-008 timers. This task qualifies them together under fault injection.
- Coverage: GENUINE_GAP for the combined matrix.
- Real boundary: `QUANSIO_TEST_POSTGRES_URL` — present (dev stack), the matrix
  ran against it.
- Acceptance: crash recovery resumes without duplicating the run; concurrent
  cancel/suspend produce exactly one typed winner; superseded generation is
  fenced with `FencedStaleGeneration`; replay precedence is order-stable.
