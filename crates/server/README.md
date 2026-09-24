# quansio-server

**Canonical owner:** `crates/server` — quansio-server: authoritative API, control, runtime, policy, effects, scheduler and artifact composition.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

The client for the Python intelligence gateway (`src/intelligence`) is the only
way the runtime reaches the intelligence plane: a separate process, over gRPC.

Build: `cargo test -p quansio-server`
