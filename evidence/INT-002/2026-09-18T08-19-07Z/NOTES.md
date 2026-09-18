# INT-002 — first live real-boundary run: the OpenAI-compatible adapter against Z.ai

## What was run

The live conformance suite (`python/tests/intelligence/test_model_gateway_live.py`) with the
OpenAI-compatible boundary configured from its three `QUANSIO_TEST_OPENAI_COMPATIBLE_*`
variables — base URL `https://api.z.ai/api/coding/paas/v4`, wire model `GLM-5.3-Flash`, and
the account's key (never written to the repository, this bundle, or any log; verified by
grep before commit). Anthropic and OpenAI remain unconfigured.

Result (`live-openai-compatible-zai.log`): every OpenAI-compatible case passed — the `text`
and `tool_call` scenarios, the compaction-free/operator-channelled request shape, and
fail-closed-without-a-credential — and it passed again on an immediate repeat.
`test_at_least_two_live_providers_pass_the_same_suite` skipped with `BLOCKED_EXTERNAL: 1 of 3
live providers configured; at least 2 are required`, which is the accurate state.

`live-openai-compatible-zai-stream.log` records the normalized stream the boundary produced:

- `text`: CALL_STARTED, 110 THINKING_SUMMARY events (the model's `reasoning_content`),
  17 DELTA, one USAGE (47 in / 129 out), STOP `END_TURN`, CALL_COMPLETED; a greeting was
  produced.
- `tool_call`: CALL_STARTED, 26 THINKING_SUMMARY, exactly one TOOL_CALL
  `fs.read {"value":"42"}`, USAGE (215 in / 38 out, `cache_read=192` — the provider's prompt
  cache hit, normalized into the usage contract), STOP `TOOL_USE`, CALL_COMPLETED.

## What had to change first (commit 88a4cfe, merged as 3445274)

Pointing the generic adapter at a hosted OpenAI-compatible service failed twice before a
token flowed, and both are defects in the adapter's notion of "compatible", not in Z.ai:

1. **Wire path.** The adapter sent `{base_url}/v1/chat/completions`; Z.ai serves
   `/api/paas/v4/chat/completions` and has no `/v1` alias (404 confirmed for both spellings
   of the versioned root). Services disagree on the version segment, so the compatible
   profile now takes the OpenAI-SDK-style base URL every such service documents — including
   the service's own version segment — and sends relative paths (`/chat/completions`,
   `/embeddings`). The first-party OpenAI adapter keeps addressing its endpoint from the root
   with the version in the path, because that version is OpenAI's. `infra/compose/README.md`
   had already documented the dev stub as `http://127.0.0.1:59020/v1`; the adapter now
   agrees with it, and `stub_catalog()` points the compatible provider at `<stub>/v1`.
2. **Operator channel.** The operator message was rendered as OpenAI's `developer` role,
   which the shared dialect does not have (`{"code":"1214","message":"Incorrect role
   information"}`). The compatible profile now renders it as a `system` message of its own,
   after the system prompt — the provider's operator channel where available (DOSSIER.md §7),
   and still never a user turn. The request-shape tests assert the channel per profile.

Both are family switches on `OpenAIAdapter` narrowed by `OpenAICompatibleAdapter`, exactly
like `max_tokens` and `parallel_tool_calls` already were. No wire behaviour of the Anthropic
or first-party OpenAI adapters changed.

3. **Verifier.** The shared verifier asserted an exact THINKING_SUMMARY count in live mode
   too, which rejects any reasoning model; the adapter already normalizes `reasoning_content`
   deltas to THINKING_SUMMARY events. Whether a model thinks is model output, not
   normalization behaviour, so `LIVE_MODE` no longer fixes the count
   (`VerificationMode.exact_thinking`); `STUB_MODE` still does.

## Gates on the merge commit

- `(cd python && uv run --frozen pytest -q)` — 444 passed, 82 skipped (`pytest-python.log`).
- ruff / ruff format / mypy — clean (`lint-type.log`).
- live suite without variables — 5 passed, 10 skipped, all `BLOCKED_EXTERNAL`
  (`live-suite-unconfigured.log`).
- `python3.12 scripts/validate_v81.py` — PASS (`validate.log`).
- `bash scripts/ci/ci.sh` — PASS (`ci.log`). One earlier run of the same commit failed only
  in `crates/server/tests/turn_loop.rs` with `duplicate key value violates unique constraint
  "protocol_states_run_id_key"` under the workspace-wide parallel `cargo test`; the suite
  then passed 3/3 alone. That is a pre-existing race in Rust code this change does not touch
  (`ci-run1-turn-loop-flake.log`), flagged for its owning task.

## Status

INT-002 stays `BLOCKED_EXTERNAL` / `implementation_complete: true`: one of the two required
live providers has now passed and is recorded in `real_boundary_evidence`. The remaining
unblock is a second adapter passing live — `QUANSIO_TEST_ANTHROPIC_API_KEY` or
`QUANSIO_TEST_OPENAI_API_KEY` — after which the acceptance test runs instead of skipping and
the task can be recorded PASS.
