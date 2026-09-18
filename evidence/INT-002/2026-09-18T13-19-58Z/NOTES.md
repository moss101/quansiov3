# INT-002 — second live adapter: the Anthropic adapter against Z.ai's Anthropic-compatible endpoint

## Decision this bundle rests on

DOSSIER.md §2.1 requires at least two adapters to pass the live conformance suite. The
product owner chose Z.ai's Anthropic-compatible endpoint (`https://api.z.ai/api/anthropic`,
serving `GLM-5.3-Flash`) as the live boundary for the Anthropic adapter, alongside the
OpenAI-compatible adapter's boundary recorded at 2026-09-18T08-19-07Z. **What this proves:**
the Anthropic adapter's wire handling — Messages SSE framing, `thinking`/`text`/`tool_use`
blocks, typed stop reasons, usage accumulation, `cache_control` placement, strict tool
`input_schema`, `x-api-key`/`anthropic-version` custody — against a real server that
implements that format, with the request shape sent unchanged. **What it does not prove:**
acceptance of that shape by `api.anthropic.com` itself, which remains the catalog's primary
endpoint. `real_boundary_evidence` names the endpoint it ran against for exactly that reason;
a run with `QUANSIO_TEST_ANTHROPIC_API_KEY` alone (no override) is the drop-in upgrade.

## What was run

Both boundaries configured at once (`live-two-providers-zai.log`): 14 passed, 3 skipped,
the skips being only the first-party OpenAI cases (`QUANSIO_TEST_OPENAI_API_KEY` unset).
`test_at_least_two_live_providers_pass_the_same_suite` ran and passed: two adapters,
Anthropic and OpenAI-compatible, passed the same live suite. Credentials are read only from
`QUANSIO_TEST_*` variables and appear in no log, bundle or registry (scanned before commit).

`live-anthropic-wire-zai-stream.log` records the normalized stream:

- `text`: CALL_STARTED, one THINKING_SUMMARY (the `thinking` block, aggregated), 9 DELTA,
  USAGE (45 in / 58 out), STOP `END_TURN`, CALL_COMPLETED.
- `tool_call`: CALL_STARTED, one THINKING_SUMMARY, exactly one TOOL_CALL
  `fs.read {"value":"42"}`, USAGE (213 in / 50 out), STOP `TOOL_USE`, CALL_COMPLETED.

The endpoint reports `input_tokens` in `message_delta` rather than `message_start`; the
adapter's usage accumulation absorbed that without change.

## Change (commit 88c… `[INT-002] let a first-party adapter run its live suite against a
compatible endpoint`, merged as 376d614)

Test infrastructure only. The first-party boundaries were fixed to the committed catalog;
optional `QUANSIO_TEST_{ANTHROPIC,OPENAI}_BASE_URL` / `_MODEL_ID` now point the same adapter
at another server that speaks its wire format. `live_catalog_for_kind()` replaces
`catalog_with_wire_model_id()` and overrides the wire model id and/or provider endpoint of
one kind's entry while capabilities, policy and the credential handle stay the catalog's.
`test_first_party_boundary_runs_as_committed_unless_overridden` proves both directions
offline. No product code path changed in this commit.

## Gates

- `(cd python && uv run --frozen pytest -q)` — 446 passed, 82 skipped (`pytest-python.log`).
- ruff / ruff format / mypy — clean (`lint-type.log`).
- live suite without variables — 7 passed, 10 skipped, all `BLOCKED_EXTERNAL`
  (`live-suite-unconfigured.log`).
- `python3.12 scripts/validate_v81.py` — PASS (`validate.log`).
- `bash scripts/ci/ci.sh` — PASS at 6913a23 (`ci.log`). The first run at 376d614
  (`ci-run1-nested-worktree-scan.log`) failed in the supply-chain gate because the scanners
  walked into a concurrent session's worktree under `.claude/worktrees/`; that is an OPS-007
  defect, fixed and recorded under `evidence/OPS-007/2026-09-18T13-37-50Z/`, and the green
  baseline was taken on the commit that carries the fix.

## Status

INT-002 → PASS: implementation complete, every acceptance statement covered by a test,
real-boundary proof for two adapters recorded, dependencies (INT-001, GOV-004) PASS,
validator PASS. Downstream tasks that were BLOCKED_EXTERNAL only on INT-002 become
dependency-ready; each still needs its own re-verification before it is recorded PASS.
