# INT-010 — re-verification on main after INT-002 reached PASS

INT-010 (`real_boundary: false`) was `BLOCKED_EXTERNAL` with `implementation_complete: true`
only because its dependency INT-002 was not PASS (DOSSIER.md §20). INT-002 is PASS as of
6725718; nothing in `python/intelligence/evaluation/` or `tests/evaluation/` changed. Every
recorded suite re-ran green on the current main: 61 evaluation tests (datasets digest-pinned,
runs content-addressed, the promotion gate blocking on a protected regression regardless of
aggregate quality gain, metrics observing behaviour), 56 CI-tooling tests, 282 repo tests, the
518-test python suite, ruff/mypy, supply-chain + architecture + validator, CI baseline PASS.

One recorded command was corrected: the supply-chain check must run on the pinned venv
(`uv run --project python`), not a bare `python3.12`, which lacks PyYAML and fails closed on
every `.yaml` manifest — the pipeline-correctness issue OPS-007 fixed on 2026-09-16.
