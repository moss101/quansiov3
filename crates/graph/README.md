# quansio-graph

**Canonical owner:** `crates/graph` — WorkGraph, AgentGraph and StateGraph stores plus GraphTransaction.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

Modules:

- `work` — `work_nodes`/`work_edges`; cycle rejection for `depends_on`/`parent_of`,
  revision compare-and-set, and `done` only through `mark_verification_passed`.
- `agent` — `agent_threads`/`agent_graph_edges`; delegation needs a visible parent in the
  same tenant/workspace, records its capability id, and calls a `DelegationNarrowingCheck`
  (RUN-005 plugs the Capability Projection algebra in).
- `runtime` — `runs`/`turns`/`steps`/`attempts`; the Run state machine including
  `SUSPENDED`, and append-only attempts.
- `batch` — one transaction and one graph-revision compare-and-set across all three
  graphs; CORE-005 wraps it to add event emission.

Build: `cargo test -p quansio-graph` (database tests need `QUANSIO_TEST_POSTGRES_URL`).
