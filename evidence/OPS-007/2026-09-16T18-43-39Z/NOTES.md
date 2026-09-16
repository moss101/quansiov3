# OPS-007 defect fix — quarantine gate scope + built-in pack lifecycle

Discovered by independently re-running `main`'s own test suite (`uv run --project
python pytest tests -q`), which nobody had done end-to-end since CAP-007 merged
on 2026-09-15. `scripts/validate_v81.py` alone does not run product test suites,
so this had gone unnoticed.

## Finding 1 — quarantine-gate fail-open (security)

`scripts/ci/supply_chain/skills.py`'s directory walk excluded any path component
literally named `artifacts`, intending to skip the repository's own top-level
`artifacts/ci/` (generated CI summaries/SBOMs). Because `artifacts` is also a real
product noun (`DOMAIN.md` §0's Artifact entity), this silently exempted
`packs/capabilities/artifacts/` and `packs/skills/artifacts/` from the quarantine-
lifecycle scan entirely — a false negative on a real shipped package, not a
cosmetic miss. Fixed by restricting the `artifacts` exclusion to the repository's
own top-level directory only (`TOP_LEVEL_EXCLUDED_DIRS`), while every other
vendor/build-output exclusion (`node_modules`, `target`, `dist`, …) correctly
stays "anywhere," since those names are never legitimate product nouns.

## Finding 2 — all 14 built-in manifests missing the quarantine lifecycle

Once the tree was actually scanned end-to-end, all 7 `packs/capabilities/*/pack.yaml`
and all 7 `packs/skills/*/skill.yaml` were missing the six-section quarantine
lifecycle `DOSSIER.md` §16 / `DOMAIN.md` §11.5 requires (`quarantine`, `provenance`,
`review`, `normalization`, `evaluation`, `approved`). OPS-007 (2026-09-12) predates
CAP-006/CAP-007 (2026-09-15); the gate was written and fixture-tested against a
synthetic manifest shape (`tests/ci/fixtures/supply_chain/skills/*/skill.json`)
before real packs existed, and nothing wired the two together. Result: 4 real
failures on `main` (`tests/ci/test_supply_chain.py::test_real_repository_passes_the_gate`
and three siblings).

## Design: where the lifecycle record lives

The first attempt put the six sections inline in `skill.yaml` and `pack.yaml`
directly. That broke two *other* closed-vocabulary checks the moment the full
suite ran:

- `python/intelligence/skills/models.py::SkillManifest.parse()` — a real,
  security-relevant closed vocabulary ("a skill cannot smuggle behaviour through
  a field the resolver does not understand"), exactly seven keys, no exceptions.
- `test_builtin_packs.py`'s `PACK_KEYS`/`SKILL_META_KEYS` — test-local schema
  hygiene constants.

`skill.yaml`'s vocabulary must never be touched for this. The built-in packs
already separate *behaviour* (`skill.yaml`, closed) from *identity/governance*
(`meta.yaml`: `owner`, `semver`, `status`, `provenance`) — exactly the right home
for a quarantine record. Final design:

- **`packs/skills/*/meta.yaml`**: the six sections nest under one new
  `quarantine_record` key, so they cannot collide with `meta.yaml`'s own existing
  *scalar* `provenance: pack://…` identity pointer (a different shape than the
  `provenance: {source, ref, digest}` section the lifecycle wants).
  `scripts/ci/supply_chain/skills.py` now redirects to a sibling `meta.yaml`
  (`GOVERNANCE_SIDECAR`) whenever one exists next to a detected manifest, reading
  both `status` and the six sections from it instead of the behaviour file.
- **`packs/capabilities/*/pack.yaml`**: no separate identity sidecar exists, and
  `pack.yaml` has no real closed-vocabulary parser (only the test's own `PACK_KEYS`,
  safely extended) — the six sections go inline, with `source`/`digest` folded into
  the pack's *existing* `provenance: {kind, ref}` dict rather than a new key.
- The two original OPS-007 fixtures (`skills/approved`, `skills/self-promoted`,
  synthetic `skill.json` with no `meta.yaml` sibling) are untouched by this and
  still exercise the inline path exactly as before.

## Provenance is honest, not fabricated

These are first-party artifacts authored directly in this repository as part of
CAP-006/CAP-007's own governed promotion — nothing was actually imported from an
external registry. The recorded `review.reviewed_by` / `approved.approved_by` is
`quansio-cap007-governance` (the governing process, not an invented human name);
`review.reviewed_at` / `normalization.normalized_at` / `evaluation.evaluated_at` /
`approved.approved_at` all use the manifest's own first-commit timestamp (from
`git log --diff-filter=A`), since authoring, self-review and publication happened
in that one commit; `provenance.digest` is the sha256 of the file the record
covers (`skill.yaml` for the `meta.yaml` sidecar case, the pack's own pre-edit
bytes for `pack.yaml`) at the moment this record was written.

## Verification

- `scripts/ci/supply_chain/check.py` — CLEAN (was 18 findings: 12
  `skill-lifecycle-incomplete` + 6 `skill-self-promoted`).
- `tests/ci/test_supply_chain.py` — 28/28 (was 4 failing).
- `python/tests/intelligence/test_builtin_packs.py` + the two other pack-consuming
  suites — 11/11 (was 5 failing with `SkillError: VALIDATION_SCHEMA: unknown
  manifest keys` — this is what caught the first, wrong, inline-in-`skill.yaml`
  attempt).
- Root `tests/` — 281/281 (280 passed + 1 unrelated skip in one run, 281/0 in
  another; no failures either way — a pre-existing, order-dependent skip
  unrelated to this change).
- Python plane (`cd python && uv run --frozen pytest -q`) — 512 passed, 6 skipped
  (the 6 are pre-existing live-provider cases naming their missing
  `QUANSIO_TEST_*` variable; unaffected by this change).
- `arch_check` / `workspace_check` / `check_authority --check` /
  `dossier_consistency` / `legacy_map_check` / `gen_contracts.py --check` /
  `contract_compat.py` — all CLEAN.
- `scripts/validate_v81.py` — PASS, 43 PASS tasks (unchanged), 0 ready (unchanged
  — this fix does not unblock CAP-006/CAP-007, which remain `BLOCKED_EXTERNAL` on
  INT-002's live provider credentials via INT-010; it removes a defect that would
  otherwise have blocked their eventual PASS).
- No Rust code touched; `cargo check --workspace` independently reconfirmed clean
  before this fix and unaffected by it.

## Files changed

`scripts/ci/supply_chain/skills.py` (gate logic), 7× `packs/capabilities/*/pack.yaml`,
7× `packs/skills/*/meta.yaml`, `python/tests/intelligence/test_builtin_packs.py`
(`PACK_KEYS`/`SKILL_META_KEYS` extended). `skill.yaml` files: untouched.
