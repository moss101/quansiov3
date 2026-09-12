# quansio-tools

**Canonical owner:** `crates/tools` — Tool contract, Tool Registry and dispatch descriptors shared by the runtime and qworkerd.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

Build: `cargo test -p quansio-tools`
