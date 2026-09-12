# NON-AUTHORITY — archived material

Everything in this directory is **historical input only**.

Files here are superseded implementation documents. They are kept readable for archaeology and
migration reference, but they are **not** implementation authority, must never be consumed as
current task authority, and must never be used to justify a design, contract, schema or task scope.

The only active implementation authority is:

1. `DOSSIER.md`
2. `DOMAIN.md`
3. `AGENTS.md`
4. `registries/tasks.json`
5. `registries/progress.json`

`IMPLEMENTATION_MASTER_PROMPT.md` is the agent bootstrap; `TASKS.md`, `registries/task-graph.json`
and `MANIFEST.json` are generated views.

`scripts/ci/check_authority.py --check` enforces this: every file under `docs/archive/` must carry a
`NON-AUTHORITY` banner, and active instruction files may only reference archived files that carry it.

## Contents

Empty at V8.1 initialization. GOV-001 found no pre-existing product or superseded implementation
authority in this repository (see `docs/review/2026-09-12-gov-001-reconciliation.md`).
