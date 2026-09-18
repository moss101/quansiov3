# INT-011 — re-verification on main after INT-002 reached PASS

## Why

INT-011 (`real_boundary: false`) was `BLOCKED_EXTERNAL` with `implementation_complete: true`
for one reason only: DOSSIER.md §20 forbids PASS while a dependency is not PASS, and INT-002
was blocked on live provider credentials. INT-002 is PASS as of 6725718; nothing in INT-011's
own paths changed. This bundle is the task's full recorded test set re-run on the current
`main` (e631fb5), which also carries INT-002's dialect changes to the OpenAI-compatible
adapter (SDK-style base URL, `/embeddings` relative path) — the embedding pipeline is exercised
through them below and is unaffected.

## Results

- `test_embed_rpc.py` — 10 passed (`embed-rpc.log`)
- `test_embedding_sources.py` — 15 passed (`embedding-sources.log`)
- `test_embeddings.py` — 15 passed (`embeddings.log`)
- `tests/integration` with `QUANSIO_TEST_POSTGRES_URL` against real PostgreSQL + pgvector and
  real MinIO — 72 passed (`integration-postgres-minio.log`)
- `test_server_launch_embed.py` — 2 passed (`server-launch-embed.log`)
- full python suite with the dev database — 518 passed, 13 skipped (`pytest-python.log`)
- ruff / ruff format / mypy — clean (`lint-type.log`)
- `tests/architecture tests/contract` (the cargo-driven regeneration check deselected, covered
  by the CI `contract-drift` gate) — 139 passed (`architecture-contract.log`)
- `python3.12 scripts/validate_v81.py` — PASS (`validate.log`)
- `bash scripts/ci/ci.sh` — PASS (`ci.log`)

Every acceptance statement holds as recorded in the 2026-09-13 bundle: rebuild equivalence,
deletion propagation, the mandatory tenant filter, and embedding provider failover.
