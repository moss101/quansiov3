# APP-014 — Teammate definitions, persona and roster

## What was built (task branch `task/APP-014-teammates`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
55 lines of Rust value helpers and 18 lines of TypeScript, with no store, no commands, no
projection, no thread provisioning and no UI. The stale branch of that era is parked as
`stale/APP-014-teammates-2026-09-15`. Ownership decisions are recorded in
`evidence/APP-014/RECONCILIATION.md`.

### Server

- `migrations/0014_teammate_definition.sql` — `template_id`, `model_route_preference`,
  `default_execution_target_id`, `availability` (`available | away`) and `version` on
  `teammates` (additive). `crates/{events,server}/build.rs` watch `migrations/` so an added
  migration is a rebuild (`sqlx::migrate!` embeds the files it saw at expansion; without
  the watch a test binary ran against a stale schema).
- `crates/server/src/control/teammates/` — `TeammateStore`: `create` from a shipped
  template (`config/teammates.yaml`, typed and validated at load) or explicit fields, with
  one ACTIVE AgentThread per teammate per workspace provisioned through RUN-002
  (`definition_id` set); `update` (versioned; needs validated as effect classes, memory
  scope and availability in their vocabularies, text bounded); `archive` (definition
  `archived`, its thread(s) `SUSPENDED` with the reason `teammate archived` through
  RUN-002's generation-fenced transition; nothing deleted); `turn_definition` (what a turn
  reads); `default_for`. Every change commits its `agent.definition_{created,updated,
  archived}` RuntimeEvent and its AuditEntry in the owner's transaction.
  `narrow_by_needs`: the policy's grants whose effect class the definition names, all of
  them when it names none, never one the policy did not grant.
- `crates/server/src/api/teammates.rs` — `GET /v1/teammates?workspace_id=` (each
  definition with its thread status/generation/suspension reason and run count, active
  run, attention = open questions + pending approvals of its runs, and the effective
  capability computed as policy ∩ needs with `needs_not_granted`; the shipped templates);
  `CreateTeammate`, `UpdateTeammate`, `ArchiveTeammate` (manage_workspace). DOMAIN.md §14
  gains `/teammates` (contracts regenerated).
- `PostMessage` routes to the named teammate (`teammate_id`) or the workspace's default
  (oldest active, available; provisioned from `general-assistant` when there is none —
  configuration, not code); an archived or away teammate refuses with `CONFLICT_STATE`;
  the teammate's default execution target binds the run. `seed_projection` records the
  `agent_definition` input (`agt_…@v<version>`, digest over the authority-relevant fields)
  and narrows the grants by the needs; persona and standing instructions are not read by
  the projection at all.
- The conformance seam records the definition it read at turn time in the route's
  `chosen_by` (`teammate:agt_…@v2;prefers:fast`), so a changed definition is visible on
  the very next turn without restart.

### Desktop (`apps/desktop/src/teammates/`)

- `roster.ts` — decoders (definition, thread, active run, attention, effective
  capability, templates), `visibleRoster`, `needsAttention`, `narrowing()` (explains the
  server's grants against the needs — met, unmet, removed — and is identical for any
  instructions text), validated `createTeammate`, `updateTeammate` (refuses an archived
  one), `archiveTeammate`, `archiveInRoster`.
- `Teammates.tsx` — the roster page: cards with thread, active run (opens the inspector),
  effective capability and its explanation, attention; templates picker; editor for every
  definition field; archive offered as a suspended thread. Bridge channels
  `teammates.list/create/update/archive`; `messages.post` carries `teammateId`.

## Acceptance

1. **A teammate's standing instructions cannot widen its capability projection** —
   `teammates.rs::a_definition_narrows_a_runs_capability_and_its_text_never_widens_it`: a
   definition whose instructions claim host writes and any network, with needs naming
   `fs.write.host` and `network.egress`, gets exactly the policy's `read.internal` and
   `fs.write.workspace`; narrowing the needs to read-only loses the write on the next run;
   clearing the needs restores the policy's grants and nothing more; a need that is not an
   effect class is refused whole. The projection's inputs name the definition and its
   version; the roster's `effective_capability` says the same. `teammates.e2e.test.ts`
   proves it through the desktop bridge; `roster.ts`'s `narrowing()` is pinned to be
   independent of the instructions text.
2. **Archiving a teammate suspends its AgentThread without losing thread/work/evidence
   lineage** — `teammates.rs::archiving_suspends_the_thread_and_keeps_every_identity` and
   the E2E: after `ArchiveTeammate` the thread is `SUSPENDED (teammate archived)` with an
   `agent.suspended` event, the conversation's messages, both runs, their timelines and the
   work graph's agent are unchanged under the same ids, the roster lists the teammate as
   archived history, and it takes no message and no further change.
3. **Configuration changes are audited and take effect on the next turn without restart**
   — `teammates.rs::a_configuration_change_is_audited_versioned_and_read_by_the_next_turn`:
   `UpdateTeammate` bumps the version, stages `agent.definition_updated` and an AuditEntry
   naming the changed fields under the acting user; the next `PostMessage` reports the new
   version, the run's projection input carries it, and the turn's route record names
   `teammate:agt_…@v2;prefers:fast`; `away` refuses new messages until `available`. The
   E2E shows the same through the bridge.

## Tests (task list)

| test | where |
|---|---|
| teammate capability narrowing test | `crates/server/tests/teammates.rs::a_definition_narrows…`; `control/teammates/mod.rs` unit test; `apps/desktop/tests/teammates.test.ts` (narrowing explanation); `teammates.e2e.test.ts` |
| archive/suspend lineage test | `teammates.rs::archiving_suspends_the_thread_and_keeps_every_identity`; `teammates.e2e.test.ts` |
| roster E2E | `apps/desktop/tests/teammates.e2e.test.ts` (create from template → thread → route → edit → next-turn effect → archive → lineage → default teammate) |

## Notes

- A real model source would build its system segment from `TeammateStore::turn_definition`
  the way the conformance seam records it; the Rust→Python model path is not wired in this
  deployable (INT-001's boundary), so the route record is where the read is visible.
- `model_route_preference` is recorded and handed to the turn; route selection is INT-003's.
- The walking skeleton's pre-definition teammate thread is still honoured for a workspace
  that has one and no definition; every new workspace gets a defined teammate.
- APP-011's knowledge tests now expect the `agent_definition` input between the policy
  and the active skills (the fixed layer order).
