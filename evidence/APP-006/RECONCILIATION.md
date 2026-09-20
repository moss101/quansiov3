# APP-006 reconciliation (2026-09-20, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `apps/desktop/src/trust/approvals.ts` (47 lines:
a local sha256 of a payload and a status predicate) with no server projection, no
command and no UI (docs/review/2026-09-18-m5-placeholder-audit.md). A stale
`task/APP-006-trust-ui` branch from that era is parked as `stale/APP-006-trust-ui-2026-09-15`.

- **Canonical owners:** `apps/desktop/src/trust/` (the surface, the task's `paths`) over
  RUN-006's approvals (`crates/server/src/policy`: `ApprovalRequestRecord` with its typed
  `ConsequencePreview`, `params_digest`, `untrusted_origin`; `ApprovalRuntime::grant_and_resume`;
  signed single-use receipts), RUN-007's Effect Ledger (`crates/server/src/effects`:
  `EffectRecord` with tier, resource, target, outcome, reconciliation, `is_unsettled`) and
  CORE-007's evidence (`EvidenceStore::fetch_bytes`, digest-verified). The HTTP projections and
  the two §14 trust commands live in `crates/server/src/api/trust.rs`.
- **Existing code, coverage PARTIAL, kept:** the whole runtime loop — a tool call whose
  policy decision is `ask` parks the run `WAITING_APPROVAL` with a durable request bound
  to the effect's `params_digest`; a grant is a signed receipt; the next turn consumes the
  receipt (binding checked against the effect's digest and generation) and dispatches;
  `supersede_if_params_changed` marks a request superseded when the params change; unsettled
  effects (`OUTCOME_UNKNOWN`, `RECONCILING`, `RECONCILIATION_MANUAL`) are never re-dispatched.
- **Gaps:** `GET /v1/approvals` (catalogued) unserved; `ApproveEffect`/`DenyEffect`
  (catalogued) unimplemented; `GET /v1/effects` shows only id/status/class; no evidence bytes
  projection; runs' capability projections are seeded with `read.internal` only, so no call
  can reach the `ask` path through the API.
- **Persistent state touched:** none new.
- **Contracts (DOMAIN.md §14):** `GET /v1/approvals?workspace_id|run_id`; `ApproveEffect
  {approval_request_id, params_digest}` — the digest the user previewed is part of the
  command and must equal the request's; a request that is not `requested` (superseded,
  expired, denied, granted) is `APPROVAL_SUPERSEDED`/`CONFLICT_STATE` and nothing is granted;
  `DenyEffect {approval_request_id}`; `GET /v1/effects?run_id|workspace_id` with the full
  record; `GET /v1/evidence/{id}/content` (digest-verified bytes; a mismatch is the typed
  `EVIDENCE_DIGEST_MISMATCH`, never bytes). The evidence content path is added to §14's
  read-projection row (additive; contracts regenerated).
- **External effects:** none. **Capabilities:** a run's seeded projection now covers the
  classes the workspace's policies allow *or ask for* (policy still decides per call), so
  the approval path is reachable. **Migrations:** none. **Real boundaries:** none.
- **Scenario:** the conformance seam gains `[[scenario:approval]]` (proposes `fs.write`,
  whose class the test policy sets to `ask`), so the approval loop runs without a live
  model; the approval signing key is set in the harness (`QUANSIO_APPROVAL_SIGNING_KEY`).

## Plan

1. Server: `api/trust.rs` — approvals projection (request, preview, digest, status, expiry,
   the effect's class/tier/resource/target, receipts), effects projection (full record),
   evidence content; `ApproveEffect` (binding check → `grant_and_resume` → next turn →
   agent reply), `DenyEffect` (deny → effect `DENIED` → resume → next turn); seeded
   projections from policy.
2. Desktop `trust/`: `approvals.ts` (decode requests, bind the previewed digest, refuse a
   changed one before the command is even sent, untrusted-origin and escalation display),
   `effects.ts` (timeline over runs' effects/steps/evidence with unresolved states kept
   visibly open), `evidence.ts` (fetch + local digest check; a mismatch is an error state),
   renderer `Trust.tsx`; bridge channels `approvals.list`, `approvals.approve`,
   `approvals.deny`, `effects.list`, `evidence.content`.
3. Tests: approval binding E2E (park → preview → approve with the previewed digest →
   dispatched and settled; a changed digest is refused by the page and by the server;
   Rust: superseded request cannot be granted), effect timeline E2E (settled and unsettled
   effects; an `OUTCOME_UNKNOWN` effect stays unresolved in the view and in the ledger),
   evidence digest error UI (Rust: tampered object → typed error; desktop: the error state
   and the happy path's local digest check).
