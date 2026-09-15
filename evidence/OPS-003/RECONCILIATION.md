# OPS-003 reconciliation

- Canonical owners: `crates/server/observability`, `python/intelligence/observability`,
  `apps/desktop/src/diagnostics`.
- Existing: DLP redaction (INT-003) is provider-bound; this task is logs/traces/bundles.
- Coverage: GENUINE_GAP.
- Persistent state: none (in-process metrics/traces).
- Contracts: correlation_id already on errors; no new DOMAIN fields.
- External effects: none.
- Real boundaries: none. PASS forbidden while APP-001 is BLOCKED_EXTERNAL.
- Rollback: bundles are derived exports.

## Plan

1. Correlation + continue_trace across runtime/intelligence/desktop.
2. Redact canaries/secrets/emails before log/trace/bundle.
3. Prometheus text metrics; bounded diagnostic bundle.
4. Tests: trace propagation, canary scan, bundle bound.
