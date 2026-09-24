# APP-001 regression — the walking skeleton never reached the gateway

**Found 2026-09-24** while reconciling CAP-008. APP-001's build item requires the walking skeleton
to go "HTTP command -> runtime -> gateway with conformance-stub or live provider -> tool proposal ->
Effect Ledger -> projection -> WebSocket event". Its acceptance requires the skeleton to pass "in
CI with the conformance-stub provider".

The implementation took a different path. `api/seams.rs::skeleton_seams` gave every `ApiState` an
in-process `ConformanceStubModel`, whose scripted replies are chosen from the trigger message. No
code composed anything else. The deployable `quansio-server` therefore never called the gateway,
and no user could get an answer from a model.

**Prerequisites fixed first (each its own regression, PASS on main):**

- **INT-001:** the Rust gateway client.
- **INT-002:**
  - assistant tool calls round-trip;
  - wire-safe tool names;
  - the production tool schemas.
- **INT-005:** `BuildContext` and the context bridge.

**Fix (canonical owner `crates/server/`):**

- **`intelligence::model_source::GatewayModelSource`**, the deployable's `ModelProposalSource`.
  For one model call it:
  1. reads the turn's canonical content: the teammate's persona and instructions (or the default
     in `config/intelligence.yaml`), the thread up to the run's trigger message, and this run's
     tool rounds with their recorded results and trust;
  2. builds the context through the Context plane and runs the runtime's boundary check;
  3. calls `FulfillModel` with exactly the rendered messages and the tools the run's capability
     projection exposes, preferring the teammate's route;
  4. turns the stream into a proposal: text, and tool calls under their registry names;
  5. records the gateway's own `mr_` route (provider, model, request class, rule), the usage, the
     stop reason and the `ctx_` projection in the ModelCall ledger.

  A typed failure (plane refusal, provider error event, deadline) fails the model step with its
  DOMAIN §15 code (`RuntimeError::Intelligence`).
- **Route on the stream.** `ModelEvent.route` is set on `call_started` and `call_completed`, as
  DOMAIN §11.1 already specified; the gateway fills it.
- **API error codes.** Runtime codes now reach the HTTP boundary as their catalog codes;
  previously every code outside a hand-picked list became `INTERNAL`, so `PROVIDER_UNAVAILABLE`
  was lost.
- **Composition.** When `QUANSIO_INTELLIGENCE_ENDPOINT` is set (TCP or unix socket, lazy
  connect), the gateway source is installed. Otherwise startup says turns are answered by the
  conformance stub, not a model. `config/intelligence.yaml` bounds the call: context budget,
  output tokens, deadline, thread window.
- **Dev stub provider.** It scripts a deterministic tool round (`[[stub:call <tool> <json>]]`,
  only for an offered tool) and answers a conversation ending in tool results.

**Proof:**

- `crates/server/tests/walking_skeleton_gateway.rs`, against the real Python process and the dev
  stub provider:
  - The main test runs the full path: PostMessage → BuildContext → FulfillModel → `fs.read`
    proposal (decoded from `fs__read`) → capability, policy and Effect Ledger → second call
    carrying the tool result → agent reply → read projections → a WebSocket event for the run.
    Two ModelCall records carry the gateway's route and distinct `ctx_` projections. A scripted
    call to a tool the run was not offered is never made.
  - A provider nobody listens on fails the model step: `PROVIDER_UNAVAILABLE` (retryable) at the
    API, and no reply is invented.
- `model_source` unit tests:
  - settings load and bounds;
  - stream → proposal;
  - error or cut-off streams fail;
  - trust from the recorded rank.
- Python: `test_call_started_and_completed_carry_the_route_that_served_the_call`, for all three
  adapters.
- `tests/integration/test_dev_stack.py`: the stub's scripted round, and its refusal to call an
  unoffered tool.
- The existing stub skeleton (`walking_skeleton.rs`), API, conversation, turn-loop, routines and
  teammates suites all pass unchanged.

**Deployable smoke (the real `quansio-server` binary, composed from the environment):**

- `smoke-stub.log`: the dev stub provider. Two gateway ModelCalls (`tool_use`, then `end_turn`),
  each with a `ctx_` projection, and the `fs.read` call completed.
- `smoke-live.log`: Z.ai `GLM-5.3-Flash` over the OpenAI-compatible wire; the key was read from
  the environment, never recorded, and the log was scanned.
  - A plain question is answered by the model.
  - A tool request makes the model call `fs.read`; the runtime dispatches it through capability,
    policy and the ledger, and the model answers from the result.
  - The tool host for `fs.*` in the deployable is still the conformance host. The model says so
    itself: it saw a stub result, not the file. Real execution-target tools are EXEC's.

This is a demonstration, not a claimed real boundary (APP-001 has `real_boundary=false`).

**Mutation checks** (`mutation-checks.log`). Each of these fails the gateway walking skeleton:

1. tool rounds not rendered;
2. no tools offered;
3. route not recorded;
4. projection not recorded;
5. error event ignored;
6. model code lost at the API.
