# INT-006 — re-verification on main after INT-011 (and INT-002) reached PASS

INT-006 (`real_boundary: false`) was `BLOCKED_EXTERNAL` with `implementation_complete: true`
only because DOSSIER.md §20 forbids PASS while a dependency is not PASS: INT-011 was blocked
on INT-002. Both are PASS as of 803f2cd. Nothing in `python/intelligence/knowledge/` changed.
Every recorded suite re-ran green on the current main: 51 unit tests; 24 + 4 + 4 integration
tests against real PostgreSQL, the real derived pgvector index and the shipped gateway
embedder; the full `tests/integration` (72) and python suite (518); ruff/mypy;
architecture/contract (139); validator; CI baseline PASS. Both acceptance statements hold as
recorded on 2026-09-13: entries are tenant-scoped and provenance-addressable, and removing a
source quarantines the knowledge derived from it and empties both retrieval views.
