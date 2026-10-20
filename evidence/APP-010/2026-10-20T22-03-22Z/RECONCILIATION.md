# APP-010 — see evidence/APP-007/2026-10-20T22-03-22Z/RECONCILIATION.md

This task's change was made in the same branch as the APP-007 live-surface work and is proved by the same gate; this bundle
names which of its logs and tests prove APP-010. Proof for APP-010: `routines.rs` `an_observing_routine_s_runs_can_read_and_report_and_nothing_else` (an observing run's projection holds only read classes where the same policy gives an acting run its writes; a full replacement keeps the limit) and `automation-triggers.test.ts` (the desktop carries the mode through an edit). Mutants Y13 and Y14.
