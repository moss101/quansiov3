# quansio-indexer

**Canonical owner:** `crates/indexer` — exact, lexical and symbol indexes over authoritative sources.

Derived, rebuildable retrieval (DOSSIER.md §9, DOMAIN.md §11.3). It owns Tantivy
files scoped per tenant, a typed `SearchProgram` execution surface with provenance
`(source_id, locator, snapshot)`, per-tenant epoch manifests for staleness detection,
and an explicit `rebuild_from` path over an `AuthoritativeSource`. It is never a
source of truth and never a second database of record: rows are read-only
projections of `ArtifactVersion` metadata plus text read from stored bytes.

The embedded engine (Tantivy) sits behind the `SearchEngine` trait, so it is
replaceable; the lexical choice is documented in `src/tantivy_engine.rs`. Semantic,
graph, history and memory channels are rejected with a typed error.

Build: `cargo test -p quansio-indexer`

Database-backed tests need the dev stack and `QUANSIO_TEST_POSTGRES_URL`, for example
`postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`; without it they print
`BLOCKED_EXTERNAL` and return.
