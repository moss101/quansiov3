# quansio-events

**Canonical owner:** `crates/events` — RuntimeEvent store, transactional outbox,
rebuildable projections and resumable client streaming.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

Build: `cargo test -p quansio-events`
