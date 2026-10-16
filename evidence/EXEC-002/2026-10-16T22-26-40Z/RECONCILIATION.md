# EXEC-002 worker composition — reconciliation

**Why this exists.** QA-002's composition gate listed `WorkerHost` as built, tested and never constructed by the served
build ("qworkerd has no daemon"). The consequence was larger than the line suggests: `fs.*`, `terminal.exec` and
`process.spawn` reached `ConformanceStubHost`, which answers `{"ok":true,"stub":"conformance"}` for any tool, so the served
product reported success for work that never happened. This change composes the worker and removes that answer.

| Item | Decision |
|---|---|
| Canonical owner | `crates/qworkerd` (daemon, executor), `crates/machine/src/gateway` (wire), `crates/server/src/worker` (hub, routed host) |
| Existing code | `WorkerHost` (EXEC-002), `FileHost`/`ProcessHost` (EXEC-006), `MachineControl` leases (EXEC-001): reused unchanged |
| Persistent state touched | none new. Leases/targets are written only through `MachineControl` |
| Contract added | worker control-channel frames — DOMAIN.md §8.3.1, same commit |
| External effects | the worker tools' existing classes (`fs_write_scope`, `process.exec.sandboxed`); no new effect class |
| Real boundaries | PostgreSQL leases, a Unix socket, a real shell and filesystem, a real child process. Nothing simulated |
| Substrate | `customer_private_worker` (outbound-only; DOMAIN §8.1) — the worker dials in, the server never dials |
| Config | `QUANSIO_WORKER_SOCKET` (server). `--socket/--tenant/--target/--workspace-root/--heartbeat-secs` (daemon) |
| Rollback | unset `QUANSIO_WORKER_SOCKET`: worker tools are refused `WORKER_UNAVAILABLE`; revert the branch |

## Design choices worth reviewing
* **No timer on the server.** Lease renewal is driven by the worker's heartbeat (DOMAIN §8.2), not a hub loop: D-004 / the
  `second-scheduler-or-timer` rule forbid a second timer. A silent worker is fenced when its lease lapses.
* **Fail closed.** No attached worker → `WORKER_UNAVAILABLE`. A dispatch sent and unanswered → `Unknown` (reconciled, never
  re-sent). A worker whose envelope does not pass its lease runs nothing.
* **Exactly once across reconnects.** The daemon keeps its `WorkerHost` (and so its record of what ran) across connections.

## Known limits (not hidden)
* **Serial dispatch.** The daemon serves one dispatch at a time; a long command holds the channel. Concurrent dispatch and
  mid-run `cancel` envelopes are a later change.
* **Unix only.** A Windows worker needs a named-pipe channel; the daemon prints that and exits 2 on non-Unix.
* **Only the five file/terminal/process tools.** `browser.*`, `scm.*`, MCP and `computer.*` are not routed through this
  worker yet. `browser.*` still reaches the wrapped host.
* **Trust boundary of the socket.** Mode `0600`: anyone running as the same user can connect and claim a registered, READY
  `customer_private_worker` target. Target registration has no API yet — only the dev seed (`--worker`) registers one.
* **Mutant E is equivalent.** "Treat the lease as held before one is granted" survives because an empty lease id still fails
  the independent lease-id guard.
* **Pre-existing, not caused here:** `completion_claims`, `routines` and `served_plan_and_memory` abort with a stack overflow
  in debug builds on `main` (reproduced with this change stashed); they pass with `RUST_MIN_STACK=16777216`. The `qworkerd`
  browser tests flaked once under load and pass alone (known).
