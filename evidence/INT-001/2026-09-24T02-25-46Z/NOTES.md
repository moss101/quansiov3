# INT-001 regression — the Rust runtime could not call the intelligence service

**Found 2026-09-24** while reconciling CAP-008. INT-001 (PASS since f1853b8) accepts
"Rust runtime can call it without shared in-process Python state", but its evidence was
Python-only. No Rust code called the service: `crates/server` did not depend on
`quansio-contracts`, and the generated tonic `IntelligenceGatewayClient` had no caller.
The statement had no test that could fail.

**What was broken along the way.** Two defects surfaced once a Rust caller existed.

- **The service crashed at startup for a chat-only catalog (INT-011 code).**
  `gateway_embedder` let the gateway's `RouteUnavailableError` escape. The composition root
  (`intelligence/server/app.py::_default_servicer`) only catches `EmbeddingError`, so a
  deployment whose catalog declares no embedding model could not start. The composition root's
  own docstring says that case is valid.
- **The dev-stack stub provider could not serve the gateway (GOV-007 code).** It answered only
  non-streaming chat completions. The gateway always streams (DOMAIN §11.1 `stream: true`), so
  every call through it ended in `PROVIDER_UNAVAILABLE` (malformed stream payload).

**Canonical owner.** INT-001 owns the service boundary, and its acceptance needs the caller's
side of it. The Rust runtime is the only authority allowed to call the intelligence plane
(DOSSIER §4.2), so the client lives in `crates/server/src/intelligence/`. That path is added to
INT-001's `paths` in the same change. No new store, service or effect path: the client uses
the existing generated contract over the existing transport.

**Fix.**

- `quansio_server::intelligence::IntelligenceClient` connects over TCP (`http://host:port`) or a
  Unix socket (`unix:///path`).
- Every call carries a `CallScope` (tenant, workspace, correlation id, deadline, capability
  projection id), sent as the `quansio-scope-bin` metadata the gateway requires. The deadline
  applies as both the gRPC timeout and a local bound.
- A refusal keeps the DOMAIN §15 code from the `quansio-error-bin` trailer.
- Exposed calls: `ClassifyTrust` and `FulfillModel` (the collected normalized event stream).
- `gateway_embedder` maps an unresolvable embedding route to `EmbeddingError(ROUTE_UNAVAILABLE)`.
  A chat-only process then starts, and `Embed` fails closed.
- The stub provider answers `stream: true` with the same deterministic completion as SSE
  `chat.completion.chunk` events: the usage chunk when requested, then `[DONE]`.

**Proof.** `crates/server/tests/intelligence_gateway.rs` starts the real Python service
(`uv run … python -m intelligence.server --transport tcp`) as a separate process. Its catalog's
only route is the OpenAI-compatible provider at the dev stub. The test then calls it from Rust:

- `ClassifyTrust`: a benign segment and an injection attempt.
- `FulfillModel`: `CALL_STARTED … DELTA … USAGE/STOP … CALL_COMPLETED` on one `mr_` route, with
  the provider's text and usage.
- Typed refusals:
  - an unscoped call is refused;
  - an unknown `route_hint` returns `ROUTE_UNAVAILABLE`;
  - a dead port returns `Unreachable`;
  - a bad scheme returns `Endpoint`.

**Other tests.**

- `python/tests/intelligence/test_embed_rpc.py::test_a_chat_only_catalog_composes_a_service_without_embed`
- `tests/integration/test_dev_stack.py::test_bootstrap_smoke` now also checks the stub's
  streaming wire.

**Mutation checks** (`mutation-checks.log`); every mutation was caught:

| Mutation | Result |
|---|---|
| Scope metadata dropped | Rust test fails |
| Error trailer ignored | Refusal test fails |
| Embedding fix reverted | Python test fails |
| Stub streaming disabled | Rust test and dev-stack test fail |

**Not in scope, and not claimed.** The deployable `quansio-server` still composes
`ConformanceStubModel` as its `ModelProposalSource`. Two pieces remain unbuilt:

- a gateway-backed proposal source with context assembly (the runtime's `ModelCallRequest`
  carries ids only);
- rendering of assistant tool calls in `RenderedMessage`.

No task's acceptance owns this composition (RUN-011 accepts a conformance-stub model).
It is raised with the user as a scope decision rather than folded into this regression fix.

**CI.** `bash scripts/ci/ci.sh` on 9ec2864 against the canonical dev stack, with pinned server
binaries: 11/11 gates PASS (`ci-summary.json`). An earlier run on e58d8ea caught two slips, both
fixed in 9ec2864:
- the test catalogs named a real catalog model id (`local-instruct`), which the
  model-identifier scan keeps in `config/`;
- `tasks.json` was edited without `--write`.
