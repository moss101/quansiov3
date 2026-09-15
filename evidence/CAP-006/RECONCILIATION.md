# CAP-006 reconciliation

- Canonical owner: `packs/skills/wiki/` — a skill pack, not a new search subsystem.
- Existing code: SearchProgram (INT-005), Knowledge Fabric (INT-006), ContextProjection
  (INT-005), skill resolver (INT-009), evaluation gate (INT-010).
- Coverage: GENUINE_GAP for the WikiSkill pack and its pinned eval campaign.
- Persistent state: none. The pack is data + a navigation procedure over existing APIs.
- Contracts: DOMAIN.md §11.3 SearchProgram, §11.4 KnowledgeEntry, §11.5 Skill.
  No new fields.
- External effects: none (`real_boundary=false`).
- Capabilities: `knowledge.cite` / `read.internal`; resolution cannot grant them.
- Migrations: none.
- Real boundaries: none. PASS is forbidden while INT-010 is BLOCKED_EXTERNAL.
- Rollback: disabling the skill (not ACTIVE) leaves SearchProgram + context packing.

## Plan

1. Skill pack manifest, instructions, pinned corpus.
2. Navigation procedure: typed SearchProgram, active-only fabric hits, provenance,
   budgeted ContextProjection.
3. Eval campaign through INT-010's gate with pack-local thresholds.
4. Tests: campaign, budget regression, disable/fallback.
