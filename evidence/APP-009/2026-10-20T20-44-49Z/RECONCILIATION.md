# APP-009 — one default teammate, duplicate threads retired

- Canonical owner: `crates/server/src/control/teammates` (teammate registry), `crates/server/src/runtime/agents` (AgentThread), `crates/server/src/api/commands.rs` (routing a message).
- Defect: a workspace's first messages could all find no teammate and each created a `general-assistant` teammate. Separately, the earlier thread race left some teammates with several threads.
- Fix: `TeammateStore::ensure_default` creates the default under a per-workspace advisory transaction lock and looks again; a loser abandons its transaction (no write, no event) and is handed the winner. `AgentStore::retire_duplicate_threads` terminates a teammate's extra threads (`agent.terminated`) once no run on them is unfinished; called from `provision_thread`.
- Persistent state: no new table, column or migration. Events: existing `agent.definition_created`, `agent.terminated`.
- Contracts: DOMAIN.md §5.1 gains the one-teammate-one-thread / one-default rule (additive).
- External effects, capabilities, real boundaries: none beyond PostgreSQL.
- Rollback: revert the merge; retired duplicate threads stay TERMINATED (their messages and runs are kept).
