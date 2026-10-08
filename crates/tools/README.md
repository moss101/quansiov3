# quansio-tools

**Canonical owner:** `crates/tools` — Tool contract, Tool Registry and dispatch descriptors shared by the runtime and qworkerd.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

`src/mcp.rs` (ADP-009) turns what an MCP server offers into external Tool declarations (`mcp.<server>.<tool>`) under
`config/mcp.yaml`'s classification; `RegistryHandle` is the shared, replaceable registry snapshot.

Build: `cargo test -p quansio-tools`

`quansio-tool-check` (bin) answers "is this proposal valid against its declaration?" with the
registry's own check, one JSON line in and out — the seam evaluation (QA-004) measures through.
