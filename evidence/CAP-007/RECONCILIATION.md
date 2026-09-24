# CAP-007 reconciliation — initial built-in skill and capability packs

**Canonical owner.** `packs/skills/` and `packs/capabilities/` (content), checked by
`python/tests/intelligence/test_builtin_packs.py` and, for tool availability in the runtime's
own registry, `crates/tools/tests/builtin_packs.rs`. Installing packs into a tenant and
resolving them at run time is CAP-005's (pack lifecycle) and INT-009's (skill registry); this
task ships the packs those consume.

**Existing code (2026-09-15, reset 2026-09-19 only because CAP-003 was not real).** Seven skill
packs (research, coding, artifacts, data-analysis, cloud-devops, business-ops, wiki) with
`skill.yaml` + `meta.yaml` (identity, OPS-007 governance record) + `evals/cases.yaml`, and seven
capability packs (`pack.yaml`). Classification: PARTIAL. What the reconciliation found:

- **Tool availability was a wildcard.** The check accepted any `connector.*`, `scm.git.*` or
  `scm.pr.*` name. The coding pack needs `scm.git.diff` and `scm.pr.create`, which are not in
  the tool registry (`config/tools.yaml`; source-control tools are EXEC-012's) — a coding
  skill that cannot resolve anywhere.
- **Capability needs were not grant templates.** DOMAIN §11.5 defines `capability_needs[]` as
  grant templates; the packs used `artifact.write`, `work.plan`, `scm.write` and
  `network.egress`, none of which is a DOMAIN §7.1 effect class, and nothing checked that a
  skill's needs cover what its tools require (`artifact.create` needs `content.upload`).
- **Provenance was not verifiable.** Skill digests match `skill.yaml`; pack digests were taken
  over the pack file's pre-edit bytes (OPS-007 note) and cannot be reproduced from content.
- **Evals** only checked "resolves with everything / excluded without a tool".

**Plan.**
- [ ] Capability needs = the DOMAIN effect classes each skill's tools require (from the
      registry's grant templates); the coding skill uses the tools that exist (`fs.read`,
      `fs.patch`, `terminal.exec` — git through the sandboxed terminal) and names no
      unavailable tool; capability packs list exactly their skill's tools.
- [ ] Evals: resolve with exactly the declared needs; excluded without a tool; excluded without
      a capability (narrowing).
- [ ] Provenance: skill digest = sha256 of `skill.yaml`; pack digest = sha256 of the canonical
      JSON of the pack without its governance record; records refreshed by
      `scripts/dev/pack_records.py` and checked by the tests.
- [ ] Tests: schema/eval (existing, extended), capability scan (effect-class vocabulary,
      coverage, no surplus, no daemon/bypass), tool availability against the Rust registry
      loader, pack↔skill consistency, digest verification; negative cases for each rule.

**Persistent state / effects / migrations.** None: packs are content. No external effects.
**Dependencies.** INT-009 PASS, CAP-003 PASS, CAP-006 BLOCKED_EXTERNAL (implementation
complete) — so this task ends BLOCKED_EXTERNAL with `implementation_complete: true`.
