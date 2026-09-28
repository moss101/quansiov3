# RUN-008 regression fix: completion claims reach the served verifier

Branch `fix/RUN-008-completion-claim-composition` (fix commit 04b1581). Found by QA-001 coverage
work (2026-09-27) and listed by the QA-002 composition gate
(`tests/architecture/test_qa002_composition.py`, KNOWN_GAPS `VerificationPort`,
`ContractVerifier`, `SemanticVerifierPort`).

## Regression

- Nothing in production ever filled `ModelProposal.completion_claim`: the gateway path
  (`crates/server/src/intelligence/model_source.rs`) yields text and tool calls only, and
  `config/tools.yaml` had no tool that claims completion.
- `chat_engine` (`crates/server/src/api/commands.rs`) never called
  `RuntimeEngine::with_verification`, so a served claim would have met
  `UnavailableVerification`; `ContractVerifier` was constructed only in tests.
- The Python semantic verifier had no RPC and no Rust `SemanticVerifierPort` implementation.
- The engine's rejection path ended the turn without the model ever seeing the feedback,
  although DOMAIN.md §5.6 says "feedback appended and loop continues (bounded)".

## Reconciliation

- Canonical owners: RUN-008 (`crates/server/src/runtime/verification/`,
  `python/intelligence/evaluation/semantic_verifier/`) and RUN-011 (`crates/tools/` via
  `config/tools.yaml`, `crates/server/src/runtime/turn_loop/`, the engine's turn loop in
  `runtime/state_machine`). The RPC method on the INT-001 client and the servicer handler
  follow the CAP-008 pattern (proto + client method + servicer + composition).
- Persistent state: no migration. `tool_calls.result` of a `work.complete` call carries the
  verdict; `runs.budget_snapshot` gains the optional `max_verification_rounds` key.
- Contracts: `schemas/proto/.../service.proto` gains `VerifyCompletion`
  (`CompletionVerificationRequest` → `CompletionVerdict`), Rust and Python bindings
  regenerated; `schemas/catalog/tools.yaml` regenerated from DOMAIN.md §7.5;
  `schemas/json/models-config.schema.json` allows `routing.verification`.
- DOMAIN.md (additive, same commit): §4.4 `VerifyCompletion`; §5.6 completion claims,
  the per-turn bound, VERIFYING recovery, conversation semantics; §7.5 `work.complete`.
- Effects: `work.complete` is `read.internal` on `{kind: work}` (tier 0): it records a claim
  and changes nothing; the runtime commits the run transition. It still passes capability,
  policy, OPS-004 admission and the Effect Ledger like every tool.
- Real boundaries: none. Model calls go to the deterministic dev stub (served tests) and the
  conformance stub (Python RPC tests).
- Rollback: reverting the merge removes the tool and the composition; no data migration.

## Design decisions (documented in DOMAIN.md §5.6)

- The claim is the `work.complete` control tool. The dispatcher validates it through the
  ToolCall protocol and hands it back as `ToolDispatchOutcome::CompletionClaimed`; the claim id
  is the `tool_call` id. A claim is dispatched after every other call of its proposal.
- Verified → `SUCCEEDED`, the claim's result is `{verified: true, run_status: SUCCEEDED}` and
  the model's text is still the turn's reply (`TurnOutcome::Succeeded.assistant_text`).
- Rejected → `VERIFYING → RUNNING`; `{verified: false, feedback, run_status: RUNNING}` becomes the
  call's result, so the next model call reads the feedback as a tool result, and the loop
  continues. `budget_snapshot.max_verification_rounds` (default 3) bounds rejected claims per
  turn; the claim that reaches the bound ends the turn with the run still RUNNING.
- A run stopped in `VERIFYING` is recovered to `RUNNING` (verification is read-only).
- Conversations: a claim means the run's WorkNode is done, never that the thread is over, and
  each message still starts its own run. A run whose WorkNode's contract binds nothing (a plain
  conversation) is never offered `work.complete`, even when policy grants it, so a chat's run
  stays open between turns.
- Semantic verification routes to the catalog's `routing.verification` model
  (`anthropic-opus`, the primary when unset). The claimant is the model of the run's latest
  recorded model call, which closes the earlier `claimant_model = None` limit. A verifier whose
  model is the claimant's refuses (POLICY_DENIED / INDEPENDENCE_UNPROVABLE); a non-verdict
  answer is VALIDATION_SCHEMA / VERDICT_MALFORMED; a failed model call is PROVIDER_UNAVAILABLE.
  Every refusal rejects the claim. Without the intelligence plane the served verifier refuses
  any contract that requires semantic verification.

## Evidence

- `crates/server/tests/completion_claims.rs` (8): five served tests (PostMessage → real Python
  process → dev stub → `work.complete` → composed ContractVerifier → `VerifyCompletion`):
  passing contract → SUCCEEDED; failing contract → feedback as tool result, second model call,
  reply, run RUNNING; verifier disagreement; claimant-equals-verifier refusal; plain
  conversation not offered the tool and stays open. Three engine-level tests: per-turn bound
  (2 and default 3), protocol refusals (capability, unknown field) never verify, VERIFYING
  recovery.
- `python/tests/intelligence/test_verify_completion_rpc.py` (10): the RPC over real gRPC with
  the real gateway and conformance stub.
- Mutation checks (`mutation_checks.py`, log `mutation.log`): seven mutants, each compiles and
  is killed by its test; the unmutated controls pass.
- Full `cargo test --workspace` after merging main: see `workspace-tests.log`.
- `bash scripts/ci/ci.sh` cannot pass in a nested worktree (contract-gen workspace detection,
  a known limit); the Rust bindings were regenerated and diffed from a scratch copy of
  `scripts/dev/contract-gen` (`RUST_BINDINGS_CURRENT`), Python/TS with `gen_contracts.check`.

## Not in scope, recorded

- A verified run does not move its WorkNode to `done` or emit `run.verification_passed`; the
  WorkNode status projection belongs to RUN-004/CORE-009.
- `test_command`, `assertion` and `citations_valid` checks still fail closed naming their
  owners (EXEC-006, RUN-008 predicate registry, CAP-002).
