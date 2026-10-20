# EXEC-002 — dispatches run side by side on a worker, and a run's cancel stops them

- Canonical owner: `crates/qworkerd` (host, daemon), `crates/machine/src/gateway/wire.rs` (wire), `crates/server/src/worker` (hub, routed host), `crates/server/src/api/commands.rs` (CancelRun/PauseRun).
- Defect: the daemon served one dispatch at a time, so a long command held the channel (heartbeats, orders and a cancel waited behind it) and nothing could stop a running command.
- Change: each dispatch runs in its own task (up to `--max-concurrent`, default 8); `ToWorker::Cancel {dispatch_token}`; `OutcomeWire::Interrupted` (settled as unknown); the hub tracks in-flight dispatches by run and `CancelRun`/`PauseRun` cancel them.
- Persistent state: none. Contracts: DOMAIN.md §8.3.1 (additive: `cancel`, `interrupted`, concurrency). No migration.
- External effects: none new; an interrupted command is an unknown outcome, never re-sent (existing rule).
- Real boundaries: real PostgreSQL, a real Unix socket, the real `qworkerd` binary and real shell processes.
- Rollback: revert the merge; an older worker ignores the unknown `cancel` frame (skipped, reported).
