# QA-009 reconciliation (2026-09-26)

- **Canonical owner:** `tests/performance/` (the task's path). Findings are fixed in their owners'
  paths on their own fix branches (CORE-003, CORE-007, CORE-008, RUN-001, RUN-010, RUN-011, OPS-004,
  APP-001, APP-009, INT-004), each with its own evidence, then merged; this task adds only
  measurement code, plus one timing test in `crates/qworkerd/tests/browser.rs`.
- **Existing code and coverage — `PARTIAL`:** `tests/performance/slos.yaml` pinned §21.2 and
  `test_slos.py` timed an in-process wiki query; the rest of it asserted that strings existed in
  source files (bounded-queue wiring, test names) and that load and soak were "reported when absent" —
  no served measurement existed. The string checks are removed; the configuration pin and the
  in-process timing stay.
- **Persistent state touched:** none by this task (scratch databases per run, dropped after).
- **Contracts/events:** none by this task; the fixes changed theirs (see each owner's evidence).
- **External effects:** a live model provider (`QUANSIO_TEST_OPENAI_COMPATIBLE_*`), twelve short
  prompts a run, the credential passed only through the gateway process's environment.
- **Real boundaries:** the deployable release build, PostgreSQL 17 and MinIO (dev stack), the dev stub
  provider, a live provider, a real Chrome. Absent → `BLOCKED_EXTERNAL`; the served runs also require
  `QUANSIO_TEST_PERF=1` (a host reserved for measurement).
- **Rollback/recovery impact:** none.
