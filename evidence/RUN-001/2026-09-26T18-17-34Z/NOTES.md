# RUN-001 — model text streams to the conversation as it is written

Found by QA-009 against DOSSIER §21.2 ("model stream first token ≤ +300 ms over the provider
baseline"): the gateway streams a model's reply (`FulfillModel` is a server stream and the provider
adapters stream), but the server collected the whole stream before continuing, and the engine
published the reply as a single `model.delta` with `final: true` after the call ended — the engine's
own comment said "a streaming source publishes per delta through the same frame", but no source did.
A person therefore saw the first token only when the last one arrived; the first-token budget could
not be met by construction.

Fix: `ModelProposalSource::propose_streaming(request, on_text)` (default: propose whole, for sources
that cannot stream — the conformance seam and test doubles); `IntelligenceClient::fulfill_model_observed`
hands each gateway event to an observer as it arrives; `GatewayModelSource` forwards every non-empty
text delta; the engine (`text_sink`) publishes each as a non-final `model.delta` live frame on the run
and workspace channels. The final frame still carries the whole text, which the desktop already uses to
replace what streamed (`apps/desktop/src/conversation/state.ts`). DOMAIN.md §9.3 now states the contract.

Tests (`tests.log`): the gateway walking skeleton — a real turn through the Python intelligence service
and the stub provider, whose reply arrives as two content deltas — asserts at least two non-final
`model.delta` frames reach a WebSocket subscriber (the conversation's participant) and that they add
up to the final text; conversation surface, runtime state machine, agents and collaboration suites;
library unit tests (`unit.log`); fmt and clippy (`lint.log`). Mutation (`mutation.log`): not forwarding
deltas fails the streaming assertion. First token against a live provider is measured by QA-009.
