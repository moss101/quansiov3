# quansio-native-windows

**Canonical owner:** `native/windows` — Windows native broker bridging UI Automation and the credential store to the Rust machine module.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

Build: `cargo test -p quansio-native-windows`
