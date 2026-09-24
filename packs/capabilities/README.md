# capability packs

**Canonical owner:** `packs/capabilities` (CAP-007). Built-in Business Capability
Packs compose skills, tools, policy needs and evals. They cannot widen caller
authority, install daemons, or bypass runtime/tool policy. Promotion/rollback
is CAP-005.
Each pack's `provenance.digest` covers its content without the governance record; after a
re-review, refresh with `uv run --project python python scripts/dev/pack_records.py --write`.
