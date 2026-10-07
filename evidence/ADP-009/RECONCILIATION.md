# ADP-009 reconciliation

Task: the decided larger surfaces (D-019): MCP client, composition sandbox, password manager and browser data import, payment-field
flow, remote control, and a record of the passkey flow. Dependencies: ADP-001 PASS, ADP-007 PASS, APP-003 PASS, **ADP-002
BLOCKED_EXTERNAL (implementation complete)**. Under D-017 this task may start, and may end only `BLOCKED_EXTERNAL` with
`implementation_complete: true`.

Base: `main` at d789f8eb (ADP-008 merged). The task branch is `task/ADP-009-larger-surfaces`.

## Canonical owners

| Concern | Owner | Where the code is |
|---|---|---|
| Tool contract, registry, external tool declarations | `crates/tools` | `src/mcp.rs`, `src/registry.rs` (`with_external`, `RegistryHandle`), `src/declaration.rs` (`ToolOrigin`) |
| Tool dispatch, nested script calls | `crates/server/src/runtime/turn_loop/dispatch.rs` | `RegistryHandle` in the dispatcher, `dispatch_nested`, `NestedOutcome` |
| MCP client (transports, restart, journal) | `crates/qworkerd/src/mcp/` | stdio and streamable HTTP; egress through `quansio_machine::egress::EgressBroker::decide` |
| Composition sandbox | `crates/qworkerd/src/compose/` | QuickJS interpreter, sessions, `compose.run` host |
| Dispatch-token journal | `crates/qworkerd/src/replay.rs` | shared by the MCP client and the sandbox |
| Password vault, import, payment fields, remote-control host | `apps/desktop/src/main/` | see the items below |

No new service, daemon, store or Go. `qworkerd` has no daemon yet (EXEC-002; QA-002 `WorkerHost` gap), so nothing here is started by a
process: the hosts are libraries the future daemon composes, and the tests compose them.

## Existing code and coverage

* `crates/tools` registry: static, built from `config/tools.yaml`. `PARTIAL`: external declarations, a replaceable snapshot and the
  `mcp.<server>.<tool>` family are new. Every built-in behaviour is unchanged (the `tool_contract`, `visibility`, `browser_side_effects`
  and `computer_receipt` suites pass untouched, except one count: the reserved families went from three to four).
* `ToolDispatchService`: held `&'static ToolRegistry`; now holds a `RegistryHandle` (`with_registry(&'static)` is kept for tests and wraps
  a clone). `GatewayModelSource` got the same handle seam. `PARTIAL`.
* `qworkerd` browser, tools, host: unchanged. `BrowserDriver::dispatch` is the real-boundary host of the sandbox's browser calls.
* Egress broker (`crates/machine/src/egress`): `EgressBroker::decide` is pure over a policy and grants; the MCP HTTP transport calls it
  through the `EgressGate` seam with a policy and grants the runtime would hand a worker (qworkerd holds no database).
* Desktop: `TokenStore`/`SecretCipher` (safeStorage) is the custody pattern the vault reuses; `local-files` is the module template.

## Persistent state touched

None. MCP servers, sandbox sessions and journals are in worker memory; the vault is one encrypted file in the user's app data beside the
session file (a device-local cache of the person's own secrets, like the session secret: DOSSIER §9 allows device-local files that are
not an authority). Every row the surfaces cause is an existing `tool_calls`, `effect_records`, `policy_decisions` or `approval_*` row.

## Contracts and events changed (DOMAIN.md, same commits as the code)

* §0: External tool, Composition script / cell (further terms are added with their items).
* §7.5: `mcp.<server>.<tool>` and `compose.run` in the tool names; "External tools (ADP-009)" and "Composition sandbox (ADP-009)".
* §12: rule 10 (external tool results and server self-description are data).
* §15: MCP (9 codes) and Compose (3 codes) families; the Rust enum, the generated catalogs, OpenAPI and SDK tables follow.
* New config with schemas: `config/mcp.yaml`, `config/composition.yaml` (named so: a file called `compose.yaml` is read by the architecture
  gate as a deployable).

## External effects and classes

An external MCP tool settles as the class `config/mcp.yaml` gives it; unclassified is `data.upload.protected` (tier 4, ask, never
`always`). `compose.run` is `read.internal` on `target:*`; each call inside a script is its own effect. No new effect class.

## Capabilities and approvals

Capability only narrows: an external tool needs a grant on `connector:mcp.<server>/<tool>` for its class; `compose.run` needs
`read.internal` on `target:*`; a script's calls are checked against the same projection one by one.

## Real boundaries and credentials

| Boundary | Variable | State on this host |
|---|---|---|
| Real MCP server process over stdio and HTTP on loopback | `python3` on `PATH` | run |
| Real PostgreSQL (dispatcher, ledger, policy) | `QUANSIO_TEST_POSTGRES_URL` (private database) | run |
| Real headless Chrome through `BrowserDriver` | Chrome at the usual path | run |
| ADP-002 native computer receipts | `QUANSIO_TEST_MACOS_COMPUTER_USE=1` | not run: dependency BLOCKED_EXTERNAL |

## Dependencies (DOSSIER §18, deny.toml, supply-chain gate)

* `rquickjs` 0.13.0 (+ `rquickjs-core`, `rquickjs-sys`): the composition sandbox's interpreter. MIT, which `deny.toml` allows; the
  QuickJS-ng engine it bundles is MIT. It adds no network, file or process access; the sandbox hands it three native functions.
  Alternatives considered: a WebAssembly runtime (needs a compiler for the script language), V8 (large, a second JIT with ambient
  surface), a hand-written interpreter (a security liability). The supply-chain gate and `cargo deny` are clean with it.
* `reqwest` in `qworkerd` (MCP streamable HTTP): already a workspace dependency (webhook delivery); rustls, no default features.
* No new Python or TypeScript dependency in the MCP and sandbox items.

## Rollback

Every surface is behind its own configuration (`enabled` in `config/mcp.yaml` and `config/composition.yaml`, the vault and import in
`config/desktop-shell.yaml` once they land) and removable by reverting the task branch.
