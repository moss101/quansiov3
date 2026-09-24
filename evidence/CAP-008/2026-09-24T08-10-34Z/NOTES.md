# CAP-008 — controlled skill evolution

**Status: BLOCKED_EXTERNAL, implementation complete.** The implementation is finished and every
acceptance statement is tested. The task still cannot be PASS: its dependency CAP-007 is
BLOCKED_EXTERNAL (implementation complete), and CAP-007 depends on CAP-006's real boundary.

**What existed.** A Python library that nothing fed or consumed:

- `patterns.py` and `candidates.py`;
- a "silent mutation" test that called a function which always raises.

**What the loop is now** (verified experience → evaluated candidate → governed promotion):

1. **Records from canonical state.** Implemented in `control::skills::SkillEvolution`. A record is
   a run that reached a verified terminal state:
   - `SUCCEEDED` → success, or recovery if a tool call failed, was rejected or went unknown on
     the way;
   - `FAILED` → failure.

   Each record carries its latest evidence (`evd_` id and digest, inline summary) and the skill
   versions its recorded ContextProjections named. That durable run→skill link exists since the
   INT-009 fix.
2. **The plane proposes.** Implemented in `python/intelligence/skills/evolution/propose.py`.
   - Patterns need `min_repeats` (`config/evolution.yaml`).
   - Each pattern becomes one CANDIDATE patch of the version the runs used: success teaches the
     verified tasks as examples; failure and recovery add what the evidence showed to the
     recovery guidance.
   - A pattern that teaches nothing new proposes nothing.
3. **The plane evaluates.** Implemented in `evaluate.py`, deterministically and with no model
   call.
   - The candidate's resolution behaviour is run against its suite: the pack's
     `evals/cases.yaml` when the manifest names one, otherwise cases synthesized from the current
     manifest.
   - It is scored as an INT-010 `EvaluationRun` (pinned `skill_behaviour` dataset) and gated by
     `evaluate_gate` with `config/evolution.yaml`.
   - The protected metrics `skill.regressions` and `skill.widened_needs` block promotion
     regardless of any other gain.
4. **The registry stores.** Each candidate becomes a CANDIDATE version (never ACTIVE), with
   provenance `{origin: evolution, from_version_id, pattern_kind, evidence[], evaluation}`. A
   semver already registered is not proposed again.
5. **Promotion is governed.**
   - `SkillStore::promote` refuses APPROVED or ACTIVE for an evolution candidate without evidence
     and a passing evaluation (`POLICY_DENIED`), whoever asks.
   - `PromoteSkillVersion` and `ProposeSkillCandidates` require `ManageTenant`. Before this,
     `PromoteSkillVersion` was open to any tenant member.

**Contracts (additive, same commits):**

- RPC `ProposeSkillCandidates`.
- §14 command `ProposeSkillCandidates {workspace_id}`.
- DOMAIN §11.5: the provenance shape, the promotion gate, and that promotion is a tenant
  administrator's command.

**Acceptance:**

- *"Production skill cannot change directly from a task run."*
  - `no_tool_a_run_can_call_writes_a_skill`: no registry tool a run can call touches skills.
  - The evolution pass leaves the ACTIVE version the one ACTIVE version.
  - Candidates never resolve.
  - An evolution candidate without a passing evaluation cannot be approved, at the registry or
    at the API.
- *"Candidate provenance links back to evidence and evaluation."* The registered candidate's
  provenance lists exactly the three failed runs' evidence ids and digests, plus the evaluation
  run id, its digest and the passing verdict.

**Tests:**

- `crates/server/tests/skill_evolution.rs`, 3 tests against the real Python plane:
  - silent mutation;
  - candidate to promotion: a member is refused, an admin promotes to APPROVED;
  - regression rejection (failed, missing, or evidence-less evaluations are refused; the only
    way on is REJECTED);
  - idempotence;
  - fail-closed without the plane (`TARGET_UNAVAILABLE`);
  - RBAC.
- `python/tests/intelligence/test_skill_evolution_proposals.py`, 9 tests:
  - patterns → evaluated candidates for each pattern kind;
  - `min_repeats` and nothing-new;
  - refusal for a skill without an ACTIVE version;
  - widened needs and lost cases are gated;
  - the synthesized suite and the pack suite;
  - the RPC over real gRPC, including a typed refusal.

**Mutation checks** (`mutation-checks.log`). Each of these mutants compiles and fails its suite:

1. regressions ignored;
2. widened needs ignored;
3. evidence dropped from provenance;
4. the evaluation gate open;
5. the candidate left as DRAFT;
6. not idempotent;
7. projections not read;
8. promotion ungoverned.

**Other changes:**

- The gate config is `config/evolution.yaml`. OPS-007's supply-chain gate reads a `skill`
  filename token as an imported skill manifest, and this file is policy, not a manifest.
- `ApiState` carries the intelligence client; composition installs it with the model source.
