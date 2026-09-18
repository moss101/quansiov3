# OPS-007 — regression: the scanners walked into a nested checkout

## Failing test and what exposed it

`tests/ci/test_supply_chain.py::test_real_repository_passes_the_gate` (and the three
real-tree tests beside it) failed on `main` at 376d614 the moment a concurrent session
created `.claude/worktrees/beautiful-grothendieck-ff31f4/` — a complete worktree of this
repository. No commit exposed it; a normal agent workflow did. The skill-lifecycle walk
(`skills.py`) reported that tree's negative fixture
`tests/ci/fixtures/supply_chain/skills/self-promoted/skill.json` as this checkout's
(`skill-lifecycle-incomplete`, `skill-self-promoted`), because the fixture exclusion is by
repository-relative path and the copy sits under `.claude/worktrees/<name>/`. The
`package.json` licence walk (`policy.py`) had the same exposure; the lockfile walker resolves
workspace members from the root manifests and did not.

## Fix (fix/OPS-007-nested-checkouts, merged as 6913a23)

`scripts/ci/supply_chain/tree.py`: `nested_checkout_roots(root)` lists every directory
strictly below the root that carries its own `.git` entry (a directory for a clone, a file
for a worktree), and both scanners skip those subtrees. The regression test
`test_nested_checkout_is_another_tree` proves the marker is what excludes them: the same
files without it are still reported, with either marker they are not, for both the skill
and the licence walk.

## Verification

- `tests/ci/test_supply_chain.py` — 29 passed (`supply-chain-tests.log`).
- `scripts/ci/supply_chain/check.py` on the real tree with the neighbouring worktree still
  present — CLEAN (`supply-chain-gate.log`).
- `bash scripts/ci/ci.sh` at 6913a23 — PASS (`ci.log`).
