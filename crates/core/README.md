# quansio-core

**Canonical owner:** `crates/core` — IDs, generation counters, idempotency keys and typed errors shared across the Quansio runtime.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

Build: `cargo test -p quansio-core`
