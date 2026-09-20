# INT-007 — Semantic memory with provenance: re-verification on main (2026-09-20)

The implementation landed in four units on 2026-09-13 (see the four earlier bundles and
`evidence/INT-007/2026-09-13T06-06-36Z/RECONCILIATION.md`); it was recorded
`BLOCKED_EXTERNAL` only because the validator requires every dependency to be `PASS` and
INT-006 was itself blocked behind INT-011/INT-002's live-provider credentials. That chain
is green now (INT-002 → INT-003 → INT-011 → INT-006 PASS, this session), so INT-007 is
re-verified in full on main (`9518eed`) and flipped.

## What was re-run

| suite | result |
|---|---|
| memory units (`test_memory_models`, `test_memory_store_boundaries`, `test_memory_candidates`, `test_memory_retrieval`) | 46 passed |
| memory against real PostgreSQL + pgvector (`test_memory_store`, `test_memory_proposals`, `test_memory_semantic_channel`) | 31 passed |
| whole python suite (`uv run --frozen pytest -q`) | 518 passed, 13 skipped |
| `ruff check`, `ruff format --check`, `mypy intelligence` | clean |

## Acceptance (unchanged, still holding)

1. *A restart succeeds with memory disabled* — memory carries no recovery state (structural
   test refusing checkpoint/cursor/lease/… fields) and the channel is an enrichment the
   retrieval path tolerates being absent.
2. *Deleting a memory removes it from every plane* — `test_memory_semantic_channel`: a
   deleted memory leaves both the row and the derived index while an unrelated memory
   still answers; an expired memory is pruned from retrieval without being deleted.
