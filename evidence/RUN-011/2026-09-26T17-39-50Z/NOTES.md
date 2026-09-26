# RUN-011 — a recorded model call is metered

Found by QA-009 (P1, cost controls): OPS-004's usage projection meters model tokens from
`model.usage_recorded` (DOMAIN.md §9.2 names the event), but no production code emitted it. The model
call ledger (`crates/server/src/runtime/turn_loop/model_calls.rs`, written by the gateway model source
and the conformance seam) stored each call's usage in `model_calls` only, so served `GET /v1/usage`
never counted a single model token — quotas and billing saw no model spend.

Fix: `ModelCallLedger::record` commits the `model_routes`/`model_calls` rows and one
`model.usage_recorded` RuntimeEvent (aggregate `model_call`, the run's workspace) in one transaction,
with `input_tokens`, `output_tokens` and `cache_tokens` (read + write) exactly as the provider reported
them. A repeated record of the same call (`ON CONFLICT (tenant_id, call_id)`) inserts nothing and emits
nothing.

Tests (`tests.log`): the gateway walking skeleton — a real chat turn through the Python intelligence
service and the stub provider — now asserts one metered record per model call, that served
`/v1/usage` totals exactly the ledger's input and output tokens, and that re-recording a call leaves
the meters unchanged; the usage, runtime state machine, observability and kill-switch suites; fmt and
clippy (`lint.log`). Mutations (`mutation.log`): not emitting the event (the transaction then rolls back
and the call row is lost — killed by the ledger assertion), and metering input tokens as zero (killed by
the usage assertion). Live-provider accuracy is measured by QA-009.
