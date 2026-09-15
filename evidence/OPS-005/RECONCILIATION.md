# OPS-005 reconciliation

- Canonical owners: `crates/server/control/recovery_point/`, `infra/dr/`.
- Existing: RUN-009 recovers a run from durable tables; this task is the
  *tenant* restore point spanning DB/events/artifacts/targets/effects.
- Coverage: GENUINE_GAP.
- Persistent state: none new. The point is a value derived from durable rows.
- Real boundary: `QUANSIO_TEST_PITR=1` for host WAL archive + object replica.
  Unset → BLOCKED_EXTERNAL. Consistency/settlement tests do not need it.
- PASS forbidden until a live PITR drill is evidenced.
