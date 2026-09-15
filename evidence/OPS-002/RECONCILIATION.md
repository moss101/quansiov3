# OPS-002 reconciliation

- Canonical owners: `crates/server/audit/`, `crates/server/control/data_lifecycle/`.
- Existing: audit module was a stub comment. Knowledge/memory forgetting already
  exists in Python (INT-006/007); this task is the control-plane deletion walk
  and the AuditEntry hash chain.
- Coverage: GENUINE_GAP.
- Persistent state: chain is in-process here; CORE-003 events remain the runtime log.
- Real boundaries: none. PASS forbidden while INT-006/007/011 are BLOCKED_EXTERNAL.
