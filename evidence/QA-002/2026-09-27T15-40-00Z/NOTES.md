# QA-002 evidence — architecture and authority consistency

## What already held

`tests/architecture/test_qa002_consistency.py` composes every shipped conformance gate on the final
tree — forbidden wiring (`scripts/ci/arch_check.py`: renderer→provider calls, Python writes to
authority tables, worker→control-database access, gateway effect execution, Rust-side tool
registration, a second scheduler or timer loop, a new deployable or persistent store without a
DOSSIER decision), the owner and dependency map (`workspace_check.py`), authority writes
(`check_authority.py`), the manifest and task DAG, and the V8.1 validator — and the per-rule
negative corpus (`test_arch_check.py`, `test_inventory.py`, `test_workspace_check.py`, …) proves each
rule catches its fixture. All of it passes (`architecture.log`, `gates.log`).

## What it could not see, and the gate that now does

Those rules catch authority that is *wrongly* wired. QA-001's coverage work found authorities that
are not wired at all: implemented, tested, and absent from the served build (startup recovery,
question expiry — both fixed — and the completion verifier, the plan and memory ports). A concern
whose only authority is never composed has no authority in the final build, so QA-002 adds
**composition reachability** (`tests/architecture/composition_scan.py`,
`tests/architecture/test_qa002_composition.py`):

- **Ports.** For every trait the workspace declares, each production implementation is a
  fail-closed default (`Unavailable*`, `No*`, `Noop*`) or real. `NOT_COMPOSED`: a real one exists
  but production never constructs it. `DEFAULT_ONLY`: production composes only the default.
- **Named authorities.** A curated list of the services and lifecycle entry points the build must
  run (engine, dispatch, budgets, delegation, questions and their expiry, startup recovery, the
  scheduler and fire executor, notifications, the usage projection, webhooks, live frames, roles,
  the tool host, the model gateway, verification, orchestration, projections, the outbox, quotas,
  KMS, computer use, the worker host), each of which production must construct or call outside its
  own module. (A heuristic over every `new` could not tell a service from a parsed value type.)
- The sources are read with `#[cfg(test)]` items, comments, string and char literals and `use`
  statements removed, and generated bindings skipped, so a mention is never mistaken for a
  construction.

The tree's findings must equal `KNOWN_GAPS` exactly: a new gap fails, and so does a closed gap still
listed. Negative corpus: a real port constructed only in tests, named only in a `use`, a doc
comment, a string or a block comment; a port on its default only; a service that only builds
itself; an entry point called only in tests; a renamed obligation; plus the clean cases (composed
port, foreign `Clone`, generated code, a composition root that defines and runs its own entry
point). Mutation runs (`mutations.log`): unwiring startup recovery, unwiring question expiry and
forgetting a known gap each fail the gate; restored, it passes.

## The gaps (open, each owned; three follow-up tasks queued)

| Gap | Owner | What is missing in the served build |
|---|---|---|
| `VerificationPort` / `ContractVerifier` | RUN-008 | the completion verifier is never installed |
| `SemanticVerifierPort` | RUN-008 | the Python semantic verifier has no RPC or Rust port |
| `PlanApplicationPort` | RUN-003 | `work.propose_plan` reaches only the unavailable port |
| `MemoryProposalPort` | INT-007 | `memory.propose` reaches only the unavailable port |
| `ProposalTrustSource` | INT-012 | proposal trust is never labelled; every tier ≥ 2 call escalates to approval |
| `WorkGraphPort` / `GraphWorkGraph` / `Orchestrator` | RUN-004 | fan-out/fan-in, the ready queue and subtree cancellation never run |
| `Projection` / `ProjectionRunner` | CORE-009 | the event-crate projections never run |
| `OutboxPublisher` | CORE-003 | no event transport; the outbox is never published |
| `admit_effect` | OPS-004 | hard quotas never refuse before an effect is reserved |
| `KmsKeyProvider` | EXEC-007 | only the local master key provider is composed |
| `ComputerDispatcher` | EXEC-010 | computer use has no composed dispatcher (task already `BLOCKED_EXTERNAL`) |
| `WorkerHost` | EXEC-002 | qworkerd has no daemon (QA-009 P-12) |

The owning tasks' own acceptance statements are library-level and still hold; what is missing is
their composition into the served product, which the follow-up tasks own. Their progress entries
carry a note.
