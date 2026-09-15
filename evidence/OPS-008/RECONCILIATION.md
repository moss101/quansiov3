# OPS-008 reconciliation

- Canonical owners: `crates/server/control/ops/`, `docs/runbooks/`.
- Existing: EFFECTS_FROZEN error code and RUN-004 zero-capacity freeze.
  This task is the operator board covering every §21.5 switch.
- Coverage: GENUINE_GAP.
- Persistent state: none new; audit entries are values on the board
  (durable audit is OPS-002).
- PASS forbidden while OPS-003 and OPS-005 are BLOCKED_EXTERNAL.
