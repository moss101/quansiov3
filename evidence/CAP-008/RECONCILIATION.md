# CAP-008 reconciliation

- Canonical owner: `python/intelligence/skills/evolution/`.
- Existing: INT-009 skill ladder/resolver, INT-010 evaluation gate, CORE-007 evidence ids.
- Coverage: GENUINE_GAP for pattern detection and candidate patches.
- Persistent state: none. This plane proposes; `crates/server` skills store promotes.
- Contracts: SkillVersion status ladder; no new DOMAIN fields.
- External effects: none.
- Real boundaries: none. PASS forbidden while INT-010 and CAP-007 are BLOCKED_EXTERNAL.
- Rollback: production skill is never written here.

## Plan

1. Detect repeated verified success/failure/recovery from evidence records.
2. Generate candidate patches with evidence+eval provenance; never ACTIVE.
3. apply_to_production refuses silent mutation.
4. propose_promotion after INT-010 gate; protected regression refuses.
