# quansio-qworkerd

**Canonical owner:** `crates/qworkerd` — Worker daemon: tool host, terminal, checkpoints and managed browser (CDP).

Owns only the responsibilities named in DOSSIER.md §5. It must not become a second runtime, store, policy engine or
effect path: all state transitions go through the canonical Quansio primitives described in `DOSSIER.md`.

- `src/bin/qworkerd.rs` dials the server's `QUANSIO_WORKER_SOCKET`, holds the lease it grants and serves the file,
  terminal and process tools (`src/executor.rs`) and, with `--chrome`/`--state-dir`, `browser.*` on a Chromium it
  launches per session (`src/browser_runtime.rs`, `src/chrome.rs`).
- `src/browser/` is the hardened driver (ADP-001): stale-page fencing, an idempotent journal with an *in doubt*
  outcome, secret placeholders. Tests drive a real headless Chrome.
- `src/mcp/` is the MCP client and `src/compose/` the composition sandbox (ADP-009): no ambient I/O, a call sent and
  not answered is in doubt, never retried.

Build: `cargo test -p quansio-qworkerd`
