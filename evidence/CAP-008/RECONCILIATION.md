# CAP-008 reconciliation — controlled skill evolution

**Canonical owner.** `python/intelligence/skills/evolution/` (the plane's evolution logic, per the
task's `paths`). Registering candidates and gating their promotion is the Skill Registry's job,
so those parts live in INT-009's registry owner (`crates/server/src/control/skills/`) and are
reached through INT-001's RPC boundary. The plane proposes; the runtime commits. No new module
owns authority.

**Existing code (reset 2026-09-19; re-reconciled 2026-09-24).** `patterns.py` and `candidates.py`
existed. Classification: PARTIAL, library-only.

- **Patterns** group records by (skill, outcome) and need at least 3 repeats.
- **Candidates** are CANDIDATE-only and carry evidence provenance.
- `apply_to_production` always raises.
- `propose_promotion` gates on INT-010.

Nothing fed or consumed this code. No records came from canonical state, and nothing ever
reached the registry. The existing "silent mutation" test calls a function that always raises,
so it cannot fail.

Since 2026-09-24 (INT-009 fix) the durable run→skill link exists:
`context_projections.segments` rows of kind `skill` name the `sklv_` versions each model call
was given.

**Persistent state.** `skill_versions` rows, written only by `SkillStore`. The provenance of an
evolution candidate is a JSON document in the existing `provenance` column:

```json
{"origin": "evolution", "from_version_id": "...", "pattern_kind": "...",
 "evidence": [{"evidence_id": "...", "digest": "...", "run_id": "..."}],
 "evaluation": {"eval_run_id": "...", "digest": "...", "passed": true,
                "reason": "...", "regressions": 0}}
```

No migration.

**Contracts (additive).**
- RPC `ProposeSkillCandidates`: the runtime sends evidence records and the ACTIVE versions; the
  plane returns evaluated candidates.
- Command `ProposeSkillCandidates` (§14, admin).
- DOMAIN §11.5 records the provenance shape and the promotion gate.

**Effects.** None external. Capabilities and approvals: the command needs a tenant or workspace
admin. Real boundaries: none of its own. The task ends BLOCKED_EXTERNAL because CAP-007 is
BLOCKED_EXTERNAL (implementation complete).

**Rollback.** Candidates are CANDIDATE rows. They never resolve and can be rejected on the
ladder.

## Plan

- [ ] **Records from canonical state (Rust).**
  - Runs in terminal verified states: SUCCEEDED → success; SUCCEEDED after a failed or
    rejected tool call → recovery; FAILED → failure.
  - Each record carries its evidence (`evd_` id and digest), and the skill versions its
    projections named.
- [ ] **Plane (Python).**
  - Detect patterns.
  - Propose one CANDIDATE patch per pattern:
    - success adds the verified task texts as examples;
    - failure and recovery add recovery guidance from the evidence summaries.
  - Evaluate each candidate deterministically against its suite. The suite is the pack's
    `evals/cases.yaml` when the manifest names one, else cases synthesized from the current
    manifest (its examples resolve; a missing tool or capability excludes it).
  - Measure regressions, and treat widened needs as protected.
  - Gate with INT-010's `evaluate_gate`; bind the eval run.
- [ ] **RPC `ProposeSkillCandidates`** served by the plane.
- [ ] **Registry (Rust).** `SkillEvolution::propose` gathers records, calls the RPC, and
  registers each candidate as DRAFT → CANDIDATE with provenance (never ACTIVE, idempotent by
  semver). `SkillStore::promote` refuses APPROVED/ACTIVE for an evolution candidate whose
  provenance has no passing evaluation.
- [ ] **Command** `ProposeSkillCandidates` (admin), with the intelligence client on `ApiState`.
- [ ] **Tests.**
  - Silent mutation: no registry tool can write skills; the command never yields ACTIVE; a
    failing candidate cannot be promoted.
  - Candidate-to-promotion: evidence → candidate with evidence+eval provenance → promoted
    through the ladder.
  - Regression rejection: a candidate that widens needs or loses a resolving case is gated,
    and its promotion is refused.
  - Mutation checks.
