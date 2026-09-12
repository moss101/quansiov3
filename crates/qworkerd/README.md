# quansio-qworkerd

**Canonical owner:** `crates/qworkerd` — Worker daemon: tool host, terminal, checkpoints and managed browser (CDP).

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

Build: `cargo test -p quansio-qworkerd`
