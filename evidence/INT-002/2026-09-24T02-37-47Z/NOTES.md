# INT-002 regression — a tool loop could not reach a first-party provider

**Found 2026-09-24** while composing the real model path for APP-001, after the INT-001 client
landed. INT-002 (PASS) normalizes "strict-schema tool calling" per DOMAIN §11.1. Its
conformance and live suites prove one round: the model calls a tool. They never proved the
second round, where the model answers from the tool's result. The second round, and the
production composition, had three defects.

1. **An assistant turn could not carry its tool calls.** The rendered-content contract allowed
   `parts`, `tool_call_id` and `tool_name` only. A tool message therefore followed an
   assistant message with no `tool_calls` (OpenAI wire) or no `tool_use` block (Anthropic
   wire). Both first-party APIs refuse that shape with 400: OpenAI says "messages with role
   'tool' must be a response to a preceding message with 'tool_calls'"; Anthropic says a
   `tool_result` block "has no corresponding tool_use block". Round 2 of every tool loop was
   unrepresentable.
2. **Registry tool names went to providers unencoded.** Registry names are namespaced
   (`fs.read`). Both first-party APIs accept only `^[a-zA-Z0-9_-]{1,64}$` in a tool name. The
   earlier live evidence ran against Z.ai's compatible endpoints, which tolerate dots; the
   first-party ones do not.
3. **The production gateway offered no tool.** `ModelGateway.from_environment` composed an
   empty `MappingToolSchemaSource`. Every tool name the runtime passed failed
   `VALIDATION_SCHEMA` ("has no registered strict schema"). Only tests, which inject
   `conformance_tool_schemas()`, could offer a tool.

**Fix** (canonical owner `python/intelligence/model_gateway/`):

- **Content contract.** `content.py`: an assistant message may carry
  `tool_calls: [{id, name, args}]` (registry names, object args). They are refused on any other
  role, and malformed or duplicate calls fail closed with `VALIDATION_SCHEMA`.
- **Name encoding.** `tooling.wire_tool_name` / `registry_tool_name` encode registry names for
  the wire (`fs.read` → `fs__read`) and decode what a provider returns.
  - A name that cannot round-trip (contains `__`, uppercase, empty segment, over 64 chars) is
    refused.
  - Decoding decides nothing about callability: an invented name decodes to a name the
    registry does not have, and the runtime rejects it (DOMAIN §7.4).
  - The event factory decodes every tool call, so a `ModelEvent.tool_call` always names the
    registry tool.
- **Adapters** render an assistant turn's calls:
  - OpenAI wire: an assistant message with `tool_calls` and null content when there is no text;
  - Anthropic: `tool_use` blocks.

  Offered tools use the wire names.
- **Production schemas.** `RegistryToolSchemaSource` is a read-only view of the same
  `config/tools.yaml` the Rust Tool Registry loads. `from_environment` composes it by default,
  so the schema offered to a model is the schema the runtime validates against. Reserved
  families (`connector.<id>.<op>`, `scm.*`) have no declaration and are never offered.
- **Conformance.**
  - The offline stub now refuses what the first-party providers refuse (bad tool names, a tool
    result without its call), so the offline suite fails where a live run would.
  - A new `tool_result` scenario (the second round) runs offline for all three adapters and in
    the live suite.

**Tests.**

- `python/tests/intelligence/test_model_gateway_tool_turns.py` (26), covering:
  - the content contract and its negatives;
  - name round-trip for every registry tool, and refusals;
  - decode of a provider's tool call;
  - the second-round wire shape per adapter;
  - provider-side refusal of a result without its call;
  - production schemas equal to `config/tools.yaml`;
  - an unreadable registry fails closed.
- `test_model_gateway_request_shape.py` now asserts the wire name.
- `STUB_SCENARIOS` / `LIVE_SCENARIOS` gain `tool_result`.

**Live (real boundary).** Z.ai, the same endpoints as the recorded INT-002 evidence:

- OpenAI-compatible at `https://api.z.ai/api/coding/paas/v4`;
- the Anthropic wire at `https://api.z.ai/api/anthropic`;
- model `GLM-5.3-Flash`;
- credentials from `QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY` and `QUANSIO_TEST_ANTHROPIC_API_KEY`,
  never recorded; both logs were scanned and contain no key material.

`tool_call` and `tool_result` pass on both wires.

- `live-run-1.log`: 15 passed, 1 failed, 4 skipped. `text-anthropic` stopped with `MAX_TOKENS`
  (reason 3) inside its 256-token budget: the reasoning model spent the budget before
  answering the greeting prompt. That scenario is untouched by this change, and it passed on
  3 reruns in isolation.
- `live-run-2.log`: 16 passed, 4 skipped. First-party OpenAI is `BLOCKED_EXTERNAL` because no
  `QUANSIO_TEST_OPENAI_API_KEY` is set.

**Not proven live:** first-party `api.openai.com` and `api.anthropic.com` (no keys). The offline
stub enforces their documented tool-name and tool-result rules instead; that is not
real-boundary evidence.

**Mutation checks** (`mutation-checks.log`); each mutation fails the tool-turn or conformance
tests:

- OpenAI assistant `tool_calls` dropped;
- Anthropic `tool_use` dropped;
- names not encoded;
- names not decoded;
- production schemas empty;
- stub accepts an invalid wire.
