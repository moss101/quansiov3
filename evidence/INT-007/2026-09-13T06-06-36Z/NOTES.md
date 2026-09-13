# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the
summary; a bundle may carry other evidence files).

## unit

1 — the memory entry model, its scopes, its closed provenance vocabulary and its lifecycle. Units 2-4
(the durable store over `public.memory_entries`, candidate creation gated by provenance and the
`ProposeMemory` RPC, and retrieval through the semantic channel with the deletion path) remain.

## recorded_decisions

- The reconciler's own invariant check is a test: the model must have nowhere to keep recovery state,
  so a structural assertion refuses `checkpoint`, `cursor`, `position`, `effect_id`, `generation`,
  `lease`, `protocol_state`, `resume_token`, `attempt` or `backoff` as fields (AGENTS.md invariant 7).
  Memory is an enrichment that can be absent, which is what makes "a restart succeeds with memory
  disabled" a property rather than a hope.
- Expiry is data, not a decision: the model carries the instant an operator set and
  `is_retrievable_at` compares `now` against it lexicographically (ISO-8601 UTC instants sort that
  way), so the model needs no clock. An expired memory is still `active` — an expiry is not a
  deletion, and the memory may be reviewed or re-proposed.
- Promotion is deliberately irreversible: `active` may only become `deleted`, because "forget this"
  is the only direction a person asks for; the model refuses the reversal rather than leaving it to
  a caller's discipline.
- The provenance vocabulary is closed to `explicit_user` and `verified_run`, and a memory must name
  its subject, so every memory is attributable to someone or something.
- The error type is `MemoryEntryError`, not `MemoryError`: the latter shadows the Python builtin, and
  a linter rule caught it. The plane is named after the entry it stores, matching INT-006's
  `KnowledgeError`/`KnowledgeEntry` pairing without the collision.
- Reconciliation recorded that the write path's seam is Rust: RUN-011's `MemoryProposalPort` with its
  default `UnavailableMemoryProposals` failing closed naming `MEMORY_OWNER`. This task supplies the
  behaviour; the cross-process binding stays APP-001's, as INT-005's context bridge and RUN-008's
  semantic verifier do.
