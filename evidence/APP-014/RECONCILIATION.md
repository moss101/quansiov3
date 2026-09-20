# APP-014 reconciliation (2026-09-21, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `crates/server/src/control/teammates/mod.rs` (55
lines: a string-list subset predicate, a value struct called "archive", a YAML id lister) and
`apps/desktop/src/teammates/roster.ts` (18 lines) — no store, no commands, no projection,
no thread provisioning, no UI (docs/review/2026-09-18-m5-placeholder-audit.md).

- **Canonical owners.** `crates/server/src/control/teammates/` (the definition: the
  `teammates` row, its lifecycle, its templates) and `apps/desktop/src/teammates/` (the
  roster), the task's `paths`. Behind them: the AgentGraph (`runtime/agents`, RUN-002:
  `AgentStore::{create_thread, activate, suspend, list_threads}`, `agent_threads.
  definition_id` already references a teammate), the capability projection seeded per run
  (`api/commands.rs::seed_projection`, CORE-004's layer order with `agent_definition`
  between the user role and delegation), the conversation owner (`control/conversation`)
  and the audit chain (`audit/store.rs`, APP-012).
- **Existing code, coverage PARTIAL, kept:** `teammates` table (CORE-001) with name,
  persona, standing_instructions, default_capability_needs, memory_scope, status;
  `config/teammates.yaml` templates; `ensure_teammate` provisions one ACTIVE teammate
  thread per workspace for `PostMessage`; the projection's `Layer::AgentDefinition`.
- **Gaps.** No definition store; `CreateTeammate`, `UpdateTeammate`, `ArchiveTeammate`
  (catalogued) unimplemented; no roster projection; the one teammate thread per workspace
  has no `definition_id` and is never suspended; the projection has no agent-definition
  input, so a definition cannot narrow anything; the table lacks the model route
  preference, default execution target and availability the build item names; nothing
  reads a definition at turn time; no audit of configuration changes.
- **Ownership decisions (recorded, not assumed).**
  - *A definition narrows, never widens* (DOMAIN.md §6.2: authority can only narrow as
    work is composed with definitions). `seed_projection` records the run's
    `agent_definition` input (the teammate id, version and a digest of the definition)
    and, when the definition declares `default_capability_needs`, keeps only the policy
    grants whose effect class the definition names; a need the policy does not grant adds
    nothing. Standing instructions and persona are text the projection never reads: they
    cannot appear in a grant by construction, and the surface test asserts a definition
    whose instructions claim more authority gets exactly what the policy ∩ needs allows.
  - *One AgentThread per teammate per workspace*, provisioned with `definition_id` when the
    teammate is created (or on first use for a workspace without one, from the
    `general-assistant` template — configuration, not code). `PostMessage` routes to the
    named teammate (`teammate_id`) or the workspace's default teammate through the same
    turn loop as before; a teammate that is `away` refuses new runs (`CONFLICT_STATE`).
  - *Archive suspends, never deletes*: `ArchiveTeammate` sets the definition `archived`
    and moves its thread(s) to `SUSPENDED` with the reason `teammate archived` through
    RUN-002's transition (generation-fenced); threads, messages, runs, work nodes and
    evidence keep their identities and are still projected. An archived teammate takes no
    new message.
  - *Configuration changes are audited and take effect on the next turn*: every command
    commits its `agent.definition_*` RuntimeEvent and its AuditEntry in one transaction,
    bumps the definition `version`, and nothing is cached — the next run's projection input
    carries the new version and digest, and the model seam reads the definition at turn
    time (the conformance stub replies in the teammate's name, so the E2E can see the
    change without a restart). The model route preference and default execution target
    are recorded and handed to the run (`execution_target_id`); route selection itself is
    INT-003's.
- **Persistent state touched:** `teammates` (+ migration `0014_teammate_definition.sql`:
  `template_id`, `model_route_preference`, `default_execution_target_id`, `availability`,
  `version` — additive), `agent_threads` (through `AgentStore`), `audit_entries`. Contract:
  `GET /v1/teammates` is added to DOMAIN.md §14's read-projection row (additive; contracts
  regenerated). Commands already catalogued.
- **External effects / real boundaries:** none.

## Plan

1. Server: migration; `control/teammates/{mod,store,templates}.rs` (`TeammateStore`:
   create from template or explicit, update, archive, list, `TurnDefinition::for_thread`);
   `api/teammates.rs` (roster projection with thread status, active run, attention and the
   effective capability computed as policy ∩ needs; the three commands, `manage_workspace`);
   `seed_projection` agent-definition layer; `PostMessage` routing by `teammate_id` /
   default teammate; the conformance seam speaks as the teammate; `config/teammates.yaml`
   completed.
2. Desktop `teammates/`: `roster.ts` (decoders, `effectiveCapability(policy, needs)`,
   `narrowing` explanation, command builders validated on the page), `Teammates.tsx`,
   bridge channels `teammates.list/create/update/archive`, the `teammates` surface.
3. Tests: `crates/server/tests/teammates.rs` rewritten as a surface test (capability
   narrowing, archive/suspend lineage, audited change effective on the next turn, away
   refusal, template instantiation); desktop unit tests and `teammates.e2e.test.ts`
   (roster E2E against the real server).
