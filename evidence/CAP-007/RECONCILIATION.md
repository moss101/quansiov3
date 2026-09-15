# CAP-007 reconciliation

- Canonical owner: `packs/skills/` and `packs/capabilities/`. Skills are
  procedural assets (INT-009); capability packs compose them with tools, policy
  and evals (DOMAIN.md §11.5–§11.6). Runtime/tool policy stays in Rust.
- Existing: `packs/skills/artifacts` (CAP-003), `packs/skills/wiki` (CAP-006).
- Coverage: GENUINE_GAP for the remaining day-one domains and capability packs.
- Persistent state: none. Packs are versioned data; promotion is CAP-005.
- Contracts: SkillManifest closed keys; PackVersion contents per DOMAIN.md §11.6.
- External effects: none.
- Capabilities: packs declare tool/capability needs and cannot grant them.
- Migrations: none.
- Real boundaries: none. PASS is forbidden while CAP-003/CAP-006 are BLOCKED_EXTERNAL.
- Rollback: packs do not install daemons; disable a skill by not resolving ACTIVE.

## Plan

1. Skill packs for research, coding, artifacts, data analysis, cloud/DevOps, business ops (plus wiki).
2. Capability packs wrapping each, with owner/version/provenance/eval.
3. Schema/eval/capability-scan/tool-availability tests.
4. Negative: daemon install, policy bypass, unknown tools, missing snapshot tools.
