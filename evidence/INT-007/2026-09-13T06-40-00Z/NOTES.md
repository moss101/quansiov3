# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the
summary; a bundle may carry other evidence files).

## unit

3 — the policy-gated candidate path and the `ProposeMemory` RPC. Unit 4 (retrieval through the
semantic channel and the deletion path) remains.

## recorded_decisions

- The two gates this module owns are the owner's, not the caller's: **provenance** (closed to
  `explicit_user` and `verified_run`, so nothing enters memory because a model found it plausible) and
  **scope** (the runtime passes a hint, as `MemoryProposalPort.scope` does; this module resolves it
  and refuses a hint it cannot satisfy rather than storing the memory somewhere else). Policy and
  capability are checked upstream by the runtime before a candidate arrives (RUN-006/RUN-011), which
  is why this module does not re-implement them.
- A scope hint that needs a workspace the call does not carry is refused, never downgraded: storing a
  workspace-scoped memory as a user one would widen who can see it.
- Re-proposal is idempotent against the claim (subject + content + provenance), and a *deleted* memory
  is not a duplicate: re-proposing something that was forgotten is a new memory, because otherwise
  forgetting would be irreversible by accident.
- The servicer is a forwarder and nothing more: it translates the wire candidate, applies the INT-001
  scope gate, and hands the candidate to the sink the composition root installs. Which store that
  sink wraps in a deployment is the composition root's decision (APP-001), exactly as INT-005's
  context bridge and RUN-008's verifier binding are; a process with no sink fails closed with a typed
  `ROUTE_UNAVAILABLE` naming that fact, so a candidate is never acknowledged while nothing stores it.
- A refused candidate stores nothing: the typed error travels back and the row count stays zero, which
  the integration suite asserts rather than assumes.
- `IMPLEMENTED_METHODS` is now `{ClassifyTrust, FulfillModel, Embed, ProposeMemory}`; the remaining
  unimplemented RPCs are `BuildContext`/`Search` (INT-005) and `Evaluate` (INT-010), and the binding
  boundary test reads the servicer's own map rather than restating it.
