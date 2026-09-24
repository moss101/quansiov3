# CAP-007 — initial built-in skill and capability packs

Reconciliation: `evidence/CAP-007/RECONCILIATION.md`. The packs from 2026-09-15 were kept;
they were reset only because CAP-003 was not real. Re-checking them against the runtime they
are meant to run in found three problems, fixed here.

## What changed

- **Tool availability is the runtime's registry, not a wildcard.** The coding pack needed
  `scm.git.diff` and `scm.pr.create`; the Tool Registry (`config/tools.yaml`, loaded by
  `quansio_tools::ToolRegistry`) cannot dispatch them (source-control tools are EXEC-012's),
  and the old check accepted any `scm.*` name. The coding skill now works through `fs.read`,
  `fs.patch` and the sandboxed `terminal.exec` (git on a work branch), and says it stops at a
  committed branch when the runtime offers no PR tool.
- **Capability needs are grant templates (DOMAIN §11.5).** They were `artifact.write`,
  `work.plan`, `scm.write`, `network.egress` — not effect classes. Each skill now declares
  exactly the effect classes its tools' registry grant templates require (artifacts →
  `content.upload`; business-ops → `record.create`, `read.internal`; cloud-devops →
  `read.internal`, `process.exec.sandboxed`; coding → `read.internal`, `fs.write.workspace`,
  `process.exec.sandboxed`; data-analysis → `read.internal`, `content.upload`; research →
  `read.external`, `read.internal`; wiki unchanged). Each capability pack lists exactly its
  skill's tools and eval suite.
- **Provenance is verifiable.** A skill's record digest is sha256 of `skill.yaml` (as before);
  a pack's is sha256 of the canonical JSON of its content without the governance record (was
  the pack file's pre-edit bytes, which nothing could reproduce). `scripts/dev/pack_records.py`
  checks and refreshes records; the changed packs' review/normalization/evaluation/approval
  times are this re-review's.
- **Evals** gain a missing-capability case per skill: resolve with exactly the declared needs,
  excluded without a tool, excluded without a capability.
- CAP-003's Python test snapshot used `artifact.write`; it now uses `content.upload`.

## Acceptance → tests

- *Every built-in pack has owner, version, provenance and passing eval.*
  `python/tests/intelligence/test_builtin_packs.py::test_pack_schema_and_eval_checks` (six
  day-one domains + wiki; owner, semver, provenance, status; each skill's eval cases run
  through the INT-009 resolver) and `test_every_governance_record_names_the_content_it_approved`
  (digests match content; an edit without re-review is stale; the record is outside the digest).
- *No pack installs daemons or bypasses runtime/tool policy.* `test_capability_scan_refuses_unknown_tools_and_daemons`
  (no daemon/bypass flags or snippets; needs are effect classes, cover every tool, no surplus;
  pack tools = skill tools) and `test_the_capability_scan_names_what_is_wrong` (negative cases).
- *Tool availability test.* `crates/tools/tests/builtin_packs.rs` — every tool a pack names is
  callable in `ToolRegistry::builtin()`, and needs are exactly the tools' grant templates.
- `mutation-checks.log`: the old coding manifest, a non-effect-class need, a surplus need, a
  stale digest and a daemon flag each fail the named tests.
- `supply-chain.log`: OPS-007's quarantine gate stays clean.

## Status

Dependencies INT-009 and CAP-003 are PASS; CAP-006 (WikiSkill) is BLOCKED_EXTERNAL on INT-010,
so this task is BLOCKED_EXTERNAL with the implementation complete. Installing packs into a
tenant and resolving them at run time are CAP-005's and INT-009's.
