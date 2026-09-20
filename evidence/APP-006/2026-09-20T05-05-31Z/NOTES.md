# APP-006 — Approvals, effects, timeline and evidence UI

## What was built (task branch `task/APP-006-trust-ui`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
a 47-line file hashing a payload locally, with no server projection, no command and no UI.
The stale branch of that era is parked as `stale/APP-006-trust-ui-2026-09-15`.

### Server (`crates/server/src/api/trust.rs`)

- `GET /v1/approvals?workspace_id|run_id[&status]` — every request with its typed
  `ConsequencePreview` (recipients, amounts, targets, data classes, the untrusted origin
  when policy escalated the proposal because its chain reached untrusted content), the
  `params_digest` the decision binds to, status and expiry, the effect it authorizes
  (class, tier, resource, target, tool and arguments) and its receipts with whether the
  single-use receipt was consumed (derived from the effect's binding).
- `GET /v1/effects?run_id|workspace_id[&status]` — the Effect Ledger record in full: class,
  tier, resource, target, the digest of what was dispatched, the outcome with its evidence
  ids, the reconciliation record, `unsettled` and `terminal` as the ledger says them.
- `GET /v1/evidence/{id}/content` — bytes verified by CORE-007 against the recorded digest
  on the way out, labelled with `x-quansio-digest`, `nosniff`, attachment. A mismatch is
  never bytes: `INTERNAL` with typed `details {kind: digest_mismatch, expected, actual}`
  (the §15 vocabulary has no integrity family; `details` is the taxonomy's typed slot).
- `ApproveEffect {approval_request_id, params_digest}` — bound to the preview: the request
  must be `requested`, unexpired, and its digest must be the presented one; otherwise
  `APPROVAL_SUPERSEDED` / `APPROVAL_EXPIRED` / `CONFLICT_STATE` and nothing is granted.
  On success RUN-006's signed single-use receipt is issued, the run is released, and its
  next turn runs in the same request — the dispatcher consumes the receipt against the
  effect's digest and generation, dispatches, settles, and the agent's reply is posted.
  `DenyEffect` denies the request, records the effect `DENIED`, releases the run and lets
  the next turn see the refusal.
- Runs' seeded capability projections now cover the classes the workspace's policies allow
  *or ask for* (policy still decides per call), so the approval path is reachable from the
  API; the conformance seam gains `[[scenario:approval]]` (proposes `fs.write`);
  `quansio-dev-seed --ask-writes` seeds the `ask` rules.

### Desktop (`apps/desktop/src/trust/`)

- `approvals.ts` — decoder, `warnings()` (untrusted origin, tier ≥ 3, money, data classes),
  `isDecidable()`, and `bindApproval(previewed, current)`: the page refuses a decision
  before it is sent when the request is not the one previewed (a different digest,
  `superseded`, `expired`, already decided, or a different request); the command carries
  the previewed digest.
- `effects.ts` — decoder, `resolution()` with `unknown`, `reconciling` and `needs_human`
  kept distinct from `succeeded`/`failed`, `unresolved()` (never empties by itself), and a
  timeline over effects and their evidence.
- `evidence.ts` — Web-Crypto sha256 on the page, `verifiedState()` (a mismatch between the
  received bytes and the labelled digest is an error state) and `errorState()` for the
  server's typed refusal.
- Bridge: `approvals.list/approve/deny`, `effects.list`, `evidence.content`; the decision
  and evidence channels return refusals as data (`{ok:false, error:{code, details}}`)
  because Electron's IPC flattens thrown errors to their message. `ApiError` now carries
  §15 `details`.
- Renderer `Trust.tsx`: pending approvals, the preview dialog (effect, resource, target,
  tool + args, consequences, expiry, digest, warnings), approve/deny bound to the preview,
  the unresolved-effects panel, the timeline, evidence with its verified digest or the
  digest-mismatch error; the shell's `approvals` surface.

## Acceptance

1. **Approval UI cannot approve changed payload after preview** — `trust.e2e.test.ts`:
   the page's `bindApproval` refuses a preview whose digest is not current before sending;
   the server refuses the same digest with `APPROVAL_SUPERSEDED` and the request stays
   `requested` with no receipt; the previewed digest is granted, its receipt is consumed by
   the dispatch and the effect settles under it; a second decision is `CONFLICT_STATE`.
   Rust `trust_surface.rs` proves the same at the API, plus deny.
2. **Unknown/unsettled effects remain visibly unresolved** — Rust: an effect marked
   `OUTCOME_UNKNOWN` is listed with `unsettled: true`, `terminal: false`, filterable by
   status; desktop `trust.test.ts`: `OUTCOME_UNKNOWN`, `RECONCILING`,
   `RECONCILIATION_MANUAL` are distinct resolutions and stay in `unresolved()`; the
   renderer shows them in their own panel and the server's flag is honoured even where the
   page would not infer it.

## Tests (task list)

| test | where |
|---|---|
| approval binding E2E | `apps/desktop/tests/trust.e2e.test.ts`; `crates/server/tests/trust_surface.rs::an_approval_is_bound_to_the_previewed_digest…`, `…a_denied_approval…` |
| effect timeline E2E | `trust.e2e.test.ts` (settled effects, receipt id on the effect, unresolved list); `trust_surface.rs::unsettled_effects_stay_visibly_unresolved`; `trust.test.ts` "effect timeline" |
| evidence digest error UI | `trust_surface.rs::evidence_bytes_that_do_not_hash…` (tampered object → typed error); `trust.test.ts` "evidence digest" (page-side verification and error states); `trust.e2e.test.ts` (verified read, typed error decoding, NOT_FOUND) |

## Limits, stated

- The in-process conformance host captures no evidence for its stub tool call, so the
  desktop E2E's evidence round-trip runs over whatever evidence the run produced (none
  with the stub) plus the error paths; the tampered-object case is proven in Rust against
  MinIO, where the test can corrupt the stored object.
- `INTERNAL` is `retryable: true` by the §15 table; the digest-mismatch `details` are what
  a client branches on to treat it as final.
