# quansio-qworkerd

**Canonical owner:** `crates/qworkerd` — Worker daemon: tool host, terminal, checkpoints and managed browser (CDP).

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

The managed browser (`src/browser/`) is driven through `BrowserDriver` (ADP-001): per-tab document
revisions, snapshot refs and captures that fail closed with `BROWSER_STALE_PAGE`, a journal that keeps
side-effecting actions idempotent and an unknown outcome *in doubt* (never retried), secret placeholders
resolved only through the runtime's secret port, and dialogs, downloads, permissions, certificates and
the user-gesture rule as typed state. Its tests drive a real headless Chrome (`BLOCKED_EXTERNAL` without one).

Build: `cargo test -p quansio-qworkerd`
