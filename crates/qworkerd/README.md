# quansio-qworkerd

**Canonical owner:** `crates/qworkerd` — Worker daemon: tool host, terminal, checkpoints and managed browser (CDP).

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

The managed browser (`src/browser/`) is driven through `BrowserDriver` (ADP-001): per-tab document revisions, snapshot
refs that fail closed with `BROWSER_STALE_PAGE`, a journal that keeps actions idempotent and an unknown outcome *in doubt*,
secret placeholders resolved only through the runtime's secret port. Its tests drive a real headless Chrome.

ADP-009 adds two hosts here: the MCP client (`src/mcp/`: servers from `config/mcp.yaml` over stdio with a cleared
environment or streamable HTTP through the egress broker's rules; classified by the configuration, results
`UNTRUSTED_EXTERNAL`; a call sent and not answered is in doubt, never retried) and the composition sandbox
(`src/compose/`: a QuickJS interpreter with no ambient I/O in which a script composes governed tool calls; a terminal
condition ends it and no `catch` can undo that). Their tests run a real MCP server and a real Chrome.

Build: `cargo test -p quansio-qworkerd`
