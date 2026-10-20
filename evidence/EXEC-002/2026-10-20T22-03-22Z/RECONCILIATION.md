# EXEC-002 — see evidence/APP-007/2026-10-20T22-03-22Z/RECONCILIATION.md

This task's change was made in the same branch as the APP-007 live-surface work and is proved by the same gate; this bundle
names which of its logs and tests prove EXEC-002. Proof for EXEC-002: `worker_live.rs` (ProvisionTarget: roles, one live target per workspace under eight concurrent asks, a worker attaching to the registered target), `worker-launcher.test.ts`, and `worker-launcher.e2e.test.ts` (the real server and the real daemon: registered, started against a folder, lease held, stopped, started again). Mutants Y6 and Y7.
