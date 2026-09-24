# APP-004 regression — the inspector could not say what the model saw

**Found 2026-09-24** right after APP-001 composed the real model path. APP-004 accepts "User can
inspect which model/context/tools were used", and the per-turn inspector
(`GET /v1/runs/{id}/timeline`, `api/threads/mod.rs`) had two problems:

- It read the turn's context from `context_projections`, but no code ever wrote that table.
- It found the projection through `turns.context_projection_id`, which is fixed at
  `begin_turn`, before any model call.

With the stub model this looked right, because the stub had no context. With the gateway
model source, every turn has a context per model call, and the inspector showed none of it.
DOSSIER §8 also lists ContextProjection as a separately persisted layer.

**Fix.**

- **`runtime::context_bridge::record_projection`.** Persists a validated projection, keyed by
  the plane's `ctx_` id: segments with kind, source reference, trust level and tokens, plus the
  token ledger `{budget, used, by_source}`, the degradation and the policy digest. Content is
  not copied; it is canonical state the runtime already holds. Recording the same id twice is
  a no-op.
- **Record before the call.** The gateway model source records the projection before the model
  call, and fails the call if it cannot record what the model would see.
- **Timeline.**
  - Each `model_call` step names its `context_projection_id`.
  - A turn without an up-front projection shows its latest call's projection, i.e. the context
    of its last answer.
  - The desktop inspector already reads `turn.context.sources`, so it needs no change.

**Proof.** `crates/server/tests/walking_skeleton_gateway.rs` covers the whole path through the
real Python process:

- the two model-call steps name the two recorded projections;
- the turn shows the latest one's sources: `system/trusted_system`, `thread/trusted_user`,
  `thread/agent_generated` (the assistant's tool call) and `tool_result/untrusted_external`,
  with a non-zero ledger;
- `context_projections` holds exactly those two rows.

The stub skeleton, the conversation surface, the context bridge, API and turn-loop suites all
pass unchanged.

**Mutation checks** (`mutation-checks.log`). Each mutant is built first, logged `COMPILES`, and
then fails the test:

1. the projection is not recorded;
2. the turn's context is not found;
3. the step's projection is hidden.
