# INT-005 regression: nothing could build the context a model call carries

**Found 2026-09-24** while composing the real model path for APP-001.

INT-005 (PASS) owns ContextProjection. DOSSIER §7 says "the Context plane owns rendering", and
DOMAIN §11.1 says a model call's `messages` are "rendered by Context plane". Neither held:

- The `BuildContext` RPC returned UNIMPLEMENTED, naming INT-005 as its owner.
- `ContextProjection` had no content: no rendered messages and no snapshot id.
- The Rust bridge had no production port and no consumer.

The Python library (`projection.py`) packs retrieval evidence into a string. A model call
needs role-structured messages that carry tool calls and their results.

**Contract (additive, DOMAIN §11.2, same commit):**

- **BuildContext request.** `snapshot_id` plus segment requests, each
  `{seq, source {kind, ref}, role, content, trust_hint?, snapshot_ref, required}`. The runtime
  supplies canonical content it owns; the Python plane reads no store.
- **Projection.** Gains `snapshot_id` and `messages[]` (RenderedMessage).
- **Ids.** `ctx_` joins the canonical prefix table (`identity.proto`, `quansio-core`); the
  plane mints it, as it mints `mr_`.

**Plane (`python/intelligence/context/turn.py`, served by `BuildContext`):**

- **Trust comes from the source.** A `trust_hint` may only lower it. Untrusted content can never
  travel in an instruction channel, and a source renders only in its roles (tool_result → tool,
  thread → user|assistant, retrieved data → user).
- **Budget.** Required segments must fit, or the build fails with `VALIDATION_BOUNDS`. A required
  unit is always kept (its cost is reserved); one whose optional members would overrun the budget
  is refused rather than split or kept over budget. The optional rest
  packs newest-first as one contiguous window of whole units: an assistant turn and the tool
  results that answer it are kept or dropped together. A window never opens on an answer
  without its question. Every drop is recorded in `degradation` with its reason, and the token
  ledger adds up.
- **Rendering.** System and operator instructions come first as the stable prefix, then the
  conversation in `seq` order. Untrusted text is rendered inside INT-012's typed boundary with
  the data-only instruction. Content is validated with the gateway's own contract, so the plane
  never renders what the gateway refuses.
- **Tool rounds.** A tool result must answer a call made by the assistant turn before it.

**Runtime (`crates/server/src/runtime/context_bridge`):**

- `ProjectionRequest` carries typed segment requests and the RPC scope.
- `validate_projection` now also requires one message per segment, each at its segment's trust
  level, so no message reaches a model more trusted than its source.
- `GatewayContextBridge` is the production port over `IntelligenceClient::build_context`. Plane
  refusals keep their code; an unreachable plane fails closed.

**Proof:**

- `python/tests/intelligence/test_turn_context.py` (31 tests):
  - ordering, labels, bounding;
  - the trust hint only lowers;
  - instruction channels refuse untrusted content;
  - role/source rules;
  - 11 malformed-request refusals;
  - tool-round pairing;
  - packing (window, units, degradation, ledger);
  - an older required turn survives a newer optional round that does not fit, and a required
    unit never overruns the budget (both fail against the first packer, which refused the
    first case and kept the second over budget);
  - required overflow;
  - the rendered messages make a valid gateway call for the Anthropic and OpenAI-compatible
    adapters;
  - BuildContext over real gRPC, including a typed refusal and an unscoped refusal.
- `crates/server/tests/intelligence_gateway.rs`: Rust builds a tool round's context through the
  real Python process and runs the boundary check. It then calls FulfillModel with exactly the
  rendered messages and `fs.read` offered (from the production registry schemas), through the
  dev stub provider. Orphan tool results and required context over budget are refused with
  their typed codes.
- `crates/server/src/runtime/context_bridge/port.rs` unit tests: message/segment trust parity and
  the segment request's JSON shape. `crates/server/tests/context_bridge.rs` is updated for the
  extended structs.

**Mutation checks** (`mutation-checks.log`): each of the eight mutations fails the named tests:

1. a hint raises trust;
2. untrusted content is not bounded;
3. tool rounds are not atomic;
4. the window may open on an answer;
5. orphan results are accepted;
6. BuildContext is unserved;
7. message trust is unchecked;
8. refusals lose their code.

**Still open, not claimed here:**

- The `Search` RPC (SearchProgram execution) is still UNIMPLEMENTED, naming INT-005. Its typed
  AST and validation exist in `python/intelligence/context/search.py`, but serving it needs the
  index-backed channels.
- Composing this bridge into the deployable server's turn loop is APP-001's walking skeleton.
