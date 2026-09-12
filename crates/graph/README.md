# quansio-graph

**Canonical owner:** `crates/graph` — WorkGraph, AgentGraph and StateGraph stores plus GraphTransaction.

Owns only the responsibilities named in DOSSIER.md §5, and never becomes a second runtime,
store, policy engine or effect path: transitions use the canonical Quansio primitives.

Modules:

- `work` — `work_nodes`/`work_edges`; cycle rejection for `depends_on`/`parent_of`, revision
  compare-and-set, and `done` only through `mark_verification_passed`.
- `agent` — `agent_threads`/`agent_graph_edges`; delegation needs a visible parent in the same
  tenant/workspace, records its capability id, and calls a `DelegationNarrowingCheck` that
  RUN-005 plugs the Capability Projection algebra into.
- `runtime` — `runs`/`turns`/`steps`/`attempts`; the Run state machine including `SUSPENDED`, and
  append-only attempts.
- `batch` — one transaction and one graph-revision compare-and-set across all three graphs.
- `transaction` — `GraphTransaction`: a batch plus its RuntimeEvents in one transaction behind
  that compare-and-set, with DOMAIN §9.2 event mapping and PlanProposal validation.
Build: `cargo test -p quansio-graph` (database tests need `QUANSIO_TEST_POSTGRES_URL`).
