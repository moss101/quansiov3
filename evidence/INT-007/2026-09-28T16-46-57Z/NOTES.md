# INT-007 — served memory.propose reaches the gated owner

Found by QA-001 coverage work (2026-09-27), fixed 2026-09-28 under the regression protocol (merge 51a3c63; evidence run on main at 51a3c63896e7, which contains every fix of this batch).

2026-09-28 (fix/INT-007-served-memory-gate, merge 51a3c63, found by QA-001 coverage): chat_engine never composed a memory port and the intelligence process composed no memory sink, so a served memory.propose failed as 'seam not available' and ProposeMemory always answered ROUTE_UNAVAILABLE. intelligence.memory.sink composes StoreMemoryProposals over memory_entries from QUANSIO_INTELLIGENCE_MEMORY_DATABASE_URL (unset: fail closed; a driver failure is a typed INTERNAL refusal without the DSN); MemoryCandidate gains scope_hint (additive); Rust IntelligenceClient::propose_memory + GatewayMemoryProposals, composed whenever the deployment has an intelligence client. The model supplies only the claim; provenance is pinned to explicit_user and the message that triggered the run; the owner stores a candidate that retrieval never returns until promoted.

Tests: cargo test -p quansio-server --test served_plan_and_memory (a_served_memory_proposal_lands_only_as_a_gated_candidate_of_the_triggering_message, a_served_memory_proposal_without_a_memory_store_is_refused_and_writes_nothing); cargo test -p quansio-server --lib provenance_is_the_runs_trigger_never_the_models; (cd python && uv run pytest tests/integration/test_memory_proposals.py) (scope hint over the wire, unreachable store typed, no DSN no sink).

Logs beside this file; controls.log reverts this task's fix on the evidence commit and shows the named test failing (KILLED) with a compiling build.
