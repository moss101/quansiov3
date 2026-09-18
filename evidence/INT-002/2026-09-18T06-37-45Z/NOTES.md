# INT-002 — extend live conformance to the OpenAI-compatible adapter

## Why

DOSSIER.md §2.1 (and this task's acceptance) requires that *at least two* of the
three adapters pass the live conformance suite; it does not say which two. The
live suite only knew Anthropic and OpenAI (`LIVE_PROVIDER_KINDS` in
`python/intelligence/model_gateway/conformance/suite.py`), so the only way to
unblock INT-002 — and the 46 tasks downstream of it — was to hold both of those
credentials. The third adapter, the generic OpenAI-compatible one that OD-004
ships precisely for self-hosted/local inference, had no live boundary at all.

## Change (test infrastructure only; no product code path changed)

- `LIVE_PROVIDER_KINDS` now covers all three adapters; `LIVE_PROVIDERS_REQUIRED = 2`
  encodes the rule.
- `catalog_with_wire_model_id()` (suite.py): the repository catalog with the wire
  model id of one provider's entry replaced. A self-hosted endpoint's model name is
  deployment data exactly like its base URL, which the catalog already takes from
  the environment for this provider; capabilities, context window, cost class and
  DLP eligibility stay the catalog's, and no model id becomes a source constant
  (D-018).
- `test_model_gateway_live.py`:
  - the OpenAI-compatible boundary reads `QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL`,
    `QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY` and
    `QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID` (documented in the module header per
    AGENTS.md "Real boundaries") and feeds the catalog's own indirection variables
    (`QUANSIO_OPENAI_COMPATIBLE_BASE_URL`, `QUANSIO_OPENAI_COMPATIBLE_KEY_HANDLE`)
    and secret handle; nothing else in the process environment is consulted.
  - `test_at_least_two_live_providers_pass_the_same_suite` is the live acceptance
    check: below two configured providers it skips with `BLOCKED_EXTERNAL` naming
    every unconfigured provider's variables; with two or more it runs the live
    scenarios for each and asserts the count of fully passing providers reaches two.
  - `test_live_gateway_fails_closed_without_a_credential` now runs for every adapter.
  - Two offline wiring checks (explicitly *not* real-boundary evidence): the boundary
    is built from the three variables alone (prepare-level), and, through the
    OpenAI-format loopback stub, the base URL, bearer credential and served model
    name reach the wire end to end (fulfil-level).

## Verification

- `(cd python && uv run --frozen pytest -q)` — 444 passed, 82 skipped
  (`pytest-python.log`).
- `(cd python && uv run --frozen pytest tests/intelligence/test_model_gateway_live.py -q -rs)`
  — 5 passed, 10 skipped, every skip carrying the `BLOCKED_EXTERNAL` marker and the
  exact variables (`live-suite-blocked-external.log`). Still no live run: no
  provider credential and no local chat model exist on this host (LM Studio has
  only an embedding model downloaded).
- ruff check / ruff format --check / mypy — clean (`lint-type.log`).
- `python3.12 scripts/validate_v81.py` — PASS (`validate.log`).
- `bash scripts/ci/ci.sh` — PASS on the merge commit (`ci.log`). Two earlier runs
  of the same commit failed only on dev-stack availability, not on code:
  `ci-run1-stack-down.log` (no Postgres at all: cargo `PoolTimedOut`, python
  `Connection refused`; `tests/integration/test_dev_stack.py` then brought the
  `quansio-dev-test` project up) and `ci-run2-canonical-stack-down.log` (the
  canonical `quansio-dev` project, whose MinIO the `quansio-server` artifact tests
  address on 59010, was not running; `.env` had been left pointing at the test
  project since 2026-09-16). `scripts/dev/up` restored the canonical stack and
  `.env`; the third run is the recorded one.
- Ad hoc (not committed): the acceptance test's three branches were driven through
  the stub standing in for two providers — two passing → passes; fewer than two
  configured → `BLOCKED_EXTERNAL` skip; one of two failing → assertion naming it.

## Effect on the blocker

INT-002 stays `BLOCKED_EXTERNAL` / `implementation_complete: true`, but the unblock
condition is now any two of: an Anthropic key, an OpenAI key, or any endpoint that
speaks the OpenAI chat-completions wire format with streaming and tool calling
(vLLM, llama.cpp, Ollama, LM Studio, TGI, or a hosted service). A run with two
configured providers in which both pass is the real-boundary evidence to record.

## Files changed

`python/intelligence/model_gateway/conformance/{suite.py,__init__.py,stub_provider.py}`
and `python/tests/intelligence/test_model_gateway_live.py`.
