# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-13 (M3 in progress: 34 PASS, INT-011 implementation complete and blocked only by
its dependency's live-credential gate, INT-002/INT-003 BLOCKED_EXTERNAL on the same credentials)
Repository: `quansiov3` (local)
Branch: `main`
HEAD: see `git rev-parse HEAD` on `main`
Authority version: V8.1

## Mission

Complete Quansio V8.1 to production readiness from the repository's own V8.1 authority set
(`AGENTS.md`, `DOSSIER.md`, `DOMAIN.md`, `registries/tasks.json`, `registries/progress.json`,
`scripts/validate_v81.py`). Persistent autonomous execution is active. Do not stop unless manually
stopped; when one task is blocked, record the blocker and take the next dependency-ready task.

## Current position

Milestone: M3 — the intelligence plane (M0/M1/M2 complete)
Current task: **`INT-010` — intelligence evaluation harness — being claimed next** (selected by
`validate_v81.py --next`: M3, `python/intelligence/evaluation/` + `tests/evaluation/`, depends on
INT-002, INT-005 and INT-009).
Previous task: **`INT-007` — semantic memory with provenance — `BLOCKED_EXTERNAL`, implementation
complete.** All four units landed: `models.py` (entry, scopes, closed provenance vocabulary, lifecycle,
one canonical instant shape), `store.py` (the durable store over `public.memory_entries` and the
tenant-bound `MemoryFabric`), `candidates.py` (the proposal type, the owner's provenance and scope
gates, and the sink `ProposeMemory` forwards to) and `retrieval.py` (the semantic channel and the
forgetting path). The reconciliation — including why "memory is not recovery" is a test rather than a
comment — is `evidence/INT-007/2026-09-13T06-06-36Z/RECONCILIATION.md`; the unit bundles are
`2026-09-13T06-29-11Z`, `2026-09-13T06-40-00Z` and `2026-09-13T06-38-37Z`, each with `NOTES.md`. Its
status is not `PASS` because its dependency INT-006 is `BLOCKED_EXTERNAL`.
Previous task: **`INT-006` — Knowledge Fabric — `BLOCKED_EXTERNAL`, implementation complete.** All
four units landed: `models.py` (entry, provenance addressing, the closed lifecycle ladder,
`quarantine_derived`), `store.py` (the durable store over `public.knowledge_entries` and the
tenant-bound `KnowledgeFabric`), `ingestion.py` (what a model may propose, and the forgetting path
through INT-011's deletion seam) and `indexing.py` (the semantic channel, kept in agreement with the
fabric in both directions). The reconciliation is
`evidence/INT-006/2026-09-13T04-58-21Z/RECONCILIATION.md`; the unit bundles are `2026-09-13T05-47-59Z`,
`2026-09-13T05-58-14Z` and `2026-09-13T06-14-00Z`, each with `NOTES.md` beside its summary. Its
status is not `PASS` because its dependency INT-011 is `BLOCKED_EXTERNAL`.
Previous task: `INT-011` — embedding pipeline and derived vector index — `BLOCKED_EXTERNAL`,
implementation complete, evidence `evidence/INT-011/2026-09-13T04-36-53Z/`. Its status is not `PASS`
because `INT-002` (its dependency) is `BLOCKED_EXTERNAL` and the validator requires every dependency
to be `PASS` first. Before that, `INT-012` and `INT-009` closed `PASS`.
Current owner: `agent:principal-1`
Current component: `knowledge` (`python/intelligence/knowledge/`)
Current language: Python

## Completed since the previous handoff

- **INT-007 (unit 4 of 4) — memory retrieval and the forgetting path.**
  `python/intelligence/memory/retrieval.py`: the channel keeps INT-011's derived index in agreement
  with the fabric in both directions (every retrievable memory indexed; nothing that left retrieval or
  expired keeps rows), a hit cites the digest of the memory's text, `retrieve` re-checks each hit
  against the fabric *and* the clock so a stale row is unservable, and `forget_memory` applies the
  model's terminal edge then clears the derived rows through INT-011's deletion seam. Memory needs no
  source reader because `memory_entries.content` stores the text. 34 rule tests plus 6 against real
  PostgreSQL and the real pgvector index.
- **INT-007 (unit 3 of 4) — the proposal gate and the `ProposeMemory` wire.**
  `python/intelligence/memory/candidates.py`: `MemoryCandidate` carries no identity, tenant or state
  (asserted structurally); the provenance vocabulary is closed to `explicit_user` and `verified_run`;
  the scope hint is resolved or refused and never downgraded, because storing a workspace memory as a
  user one would widen who can see it; re-proposing the same claim reuses the memory while a
  forgotten one is new memory. `ProposeMemory` is implemented on the servicer as a forwarder over the
  sink the composition root installs, failing closed with a typed `ROUTE_UNAVAILABLE` when none is
  configured. 12 gate tests plus 7 driving the shipped RPC over loopback onto real PostgreSQL.
- **INT-007 (unit 2 of 4) — the durable memory store.** `python/intelligence/memory/store.py`:
  `SqlMemoryStore` over `public.memory_entries` plus the tenant-bound `MemoryFabric`. The tenant is
  intrinsic; lifecycle is decided by the model and every `UPDATE` is guarded on the state the caller
  read; retrieval is one predicate that includes the clock (a candidate, a deleted memory and an
  expired one are all excluded, evaluated in SQL against an instant the caller supplies, so a wrong
  clock inside the plane cannot make memory retrievable); and `delete` retains the row. Instants
  gained one canonical shape (`memory.instant`) because `TIMESTAMPTZ` reads back as a `datetime` and
  the lexicographic expiry comparison would otherwise be unsound. `READ_TABLES` is exported and a
  structural test proves every statement names only `memory_entries` — memory is not recovery, and
  the read set proves it. 12 boundary tests plus 18 against real PostgreSQL.
- **INT-007 (unit 1 of 4) — the memory entry model.** `python/intelligence/memory/models.py`:
  scopes `user|workspace|teammate`, the closed provenance vocabulary `explicit_user|verified_run`, and
  the lifecycle `candidate → active → deleted`, taken from DOMAIN.md §11.4, CORE-001's
  `public.memory_entries` and the generated `MemoryEntry` contract. Memory is not recovery, and a
  structural test says so: the model may not carry `checkpoint`, `cursor`, `position`, `effect_id`,
  `generation`, `lease`, `protocol_state`, `resume_token`, `attempt` or `backoff`. Everything starts
  as a candidate, only `active` answers retrieval, promotion is irreversible and deletion is terminal;
  expiry is data compared lexicographically rather than a deletion. 9 tests.
- **INT-006 (unit 4 of 4) — the semantic channel.** `python/intelligence/knowledge/indexing.py`:
  `KnowledgeIndexer.synchronize` indexes every retrievable entry under the entry's identity with its
  version as the snapshot, and prunes the derived index down to the entries the fabric still
  considers retrievable — so nothing that left retrieval is reachable by meaning. An entry whose text
  cannot be read keeps the rows it had and is reported; pruning never means "the source was
  momentarily unreadable". `retrieve` re-checks every hit against the entry's lifecycle instead of
  trusting the index, and `DescribedKnowledgeText` reads text through INT-011's digest-verifying
  object reader. 14 rule tests plus 4 against real PostgreSQL and the real pgvector index.
- **INT-006 (unit 3 of 4) — ingestion and the forgetting path.** `python/intelligence/knowledge/ingestion.py`:
  a `KnowledgeProposal` carries no identity, tenant, scope or lifecycle state (asserted
  structurally), so a model may only propose; both evaluated origins (`approved_source`,
  `verified_run`) are accepted and the vocabulary is closed; re-ingesting the same claim reuses the
  entry while a retired claim is new knowledge; and forgetting a source quarantines the knowledge
  derived from it first (committed, and it stands even when the derived index is unreachable), then
  empties the source's own index rows and the rows of every entry the deletion quarantined. Two
  repository-gate findings were fixed rather than worked around: canonical identity seeding moved
  into `scripts/dev/seed_test_database.py` (the architecture gate rejects Python writing `tenants`;
  the tool also refuses a non-canonical ULID, which caught a non-Crockford id in an earlier fixture),
  and the evidence summaries were rewritten to the canonical schema with the narrative in `NOTES.md`.
- **INT-006 (unit 2 of 4) — the durable knowledge store.** `python/intelligence/knowledge/store.py`:
  `SqlKnowledgeStore` over `public.knowledge_entries` (one transaction per operation, the tenant
  context set on every one) and `KnowledgeFabric`, the tenant-bound object that is the only way to
  reach the table. The tenant is intrinsic and the database's own row-level security is the second
  check; lifecycle moves go through the model's `with_status` and every `UPDATE` is guarded on the
  state the caller read, so an illegal edge and a concurrent move are both refused; a batch of moves
  is one transaction, so a deletion is never half applied; and `delete` is a terminal lifecycle edge
  that retains the row, so provenance and audit survive. 24 database-backed tests against real
  PostgreSQL plus 9 fail-closed boundary tests that prove a refused call reaches no I/O.
- **INT-006 (unit 1 of 4) — the knowledge entry model.** `python/intelligence/knowledge/models.py`:
  a `kn_`-identified entry scoped `tenant|workspace|pack` carrying kind, content_ref, provenance,
  confidence, version, status and superseded_by, taken from DOMAIN.md §11.4 and the already-generated
  `KnowledgeEntry` contract rather than invented. An entry with no provenance reference is refused
  (provenance-addressability), `quarantine_derived` quarantines exactly the entries derived from a
  deleted source while accounting for every entry and deleting nothing, the lifecycle is a closed
  ladder whose illegal edges are refused naming both ends, deletion is terminal, and retrieval is one
  predicate — only `active` knowledge is retrievable. 13 tests; the plane suite is 304 passed /
  6 skipped; lint, format and mypy clean.
- **INT-011 (1/3) — the `Embed` wire.** `python/intelligence/model_gateway/embeddings.py` answers the
  `embedding` request class with a **non-streaming** fulfilment through the one gateway: the same
  deterministic route resolution, credential custody, DLP guard and pinned transport as a chat call,
  with only the response shape differing. `ModelGateway.embed` resolves the class through INT-003's
  `PolicyRouteSelector` (the conversation selector is class-blind and would send an embedding call to
  a chat model) and an embedding route must declare `embedding_dimensions`, so a route's width is the
  deployment's statement and the index pin can be checked against it. `embeddings/gateway_provider.py`
  joins that to the index: one `GatewayEmbeddingProvider` per catalog route, composed by
  `gateway_embedder` into a `FailoverEmbedder` pinned to `INDEX_DIMENSIONS`, so a route of another
  width is refused at composition, before any call. The servicer's `Embed` validates the scope, the
  request shape and any named route, then returns one vector per input at the width actually served;
  `IMPLEMENTED_METHODS` is now `{ClassifyTrust, FulfillModel, Embed}`.
- **INT-011 (2/3) — the authoritative rebuild.** `embeddings/sources.py` reads the authoritative
  source plane through two ports (a listing the source's owner supplies, and read-only object bytes):
  `ArtifactObjectListing` implements the CORE-007 object-key layout, `embeddings/objectstore.py`
  implements the bytes with a stdlib SigV4 GET (list + get only — writing artifact bytes is the Rust
  authority's job). Every recorded sha256 is verified before a chunk is embedded, so corruption is
  refused and nothing is written; content that cannot be a text source is **skipped and recorded**
  with the rule that fired, so coverage is auditable. `rebuild_from_sources` re-indexes every named
  source and prunes exactly that source kind's rows the set no longer names, so a source deleted
  upstream stops answering retrieval.
- **INT-011 (3/3) — the deletion seam.** `IndexSourceDeletion` is the port INT-006 and INT-007 call
  when a source is deleted. They are `NOT_STARTED`, so nothing calls it yet; that invocation is
  deferred by dependency, not by choice.
- **Defect found and fixed while wiring the endpoint.** `config/models.yaml` gave the `openai`
  provider a versioned `base_url` (`https://api.openai.com/v1`) while the OpenAI adapter's chat path
  also carries `/v1`, so a live OpenAI call would have addressed `/v1/v1/chat/completions`. The
  catalog's `base_url` is now the endpoint root for both wire families and every path carries its own
  version segment (`/v1/messages`, `/v1/chat/completions`, `/v1/embeddings`).

## Current implementation (INT-011)

The derived index is the semantic channel the Rust indexer already points at
(`crates/indexer`: `Channel::Semantic → python/intelligence/embeddings`). It is derived and
rebuildable: rows live in `derived.embeddings`, keyed by tenant, workspace, source kind, source ref,
model, snapshot, content digest and chunk index. Chunking is deterministic on paragraph, then
sentence, then word boundaries, keyed by the sha256 of the chunk text, so an edit re-embeds its tail
rather than the document. Cross-tenant retrieval is impossible by construction: an `EmbeddingIndex`
is bound to one tenant, no public method accepts one, every statement carries the filter and the
table's forced row-level security gets a context on every operation.

What is verified, on which path:

| Path | Proof |
|---|---|
| `Embed` over the real loopback gateway | 10 tests (`tests/intelligence/test_embed_rpc.py`) |
| Authoritative source plane and deletion seam | 15 tests (`tests/intelligence/test_embedding_sources.py`) |
| Index rules | 15 tests (`tests/intelligence/test_embeddings.py`) |
| Real PostgreSQL + pgvector + real MinIO | 9 tests (`tests/integration/`) |
| The shipped entry point, launched twice | 2 tests (`tests/intelligence/test_server_launch_embed.py`) |

## Exact next action

Claim and reconcile **`INT-010` — intelligence evaluation harness** (`python/intelligence/evaluation/`,
`tests/evaluation/`): versioned datasets for route quality, retrieval, answer grounding, tool-proposal
validity and skill behaviour; each run recording the model/provider/index/skill versions with cost and
latency; and the provisional thresholds from DOSSIER.md §21.3 encoded as `tests/evaluation/thresholds.yaml`.
Its two acceptance statements drive the design — an evaluation run is reproducible from pinned inputs,
and a protected safety/recovery regression blocks promotion regardless of aggregate quality gain.
Record the reconciliation, then implement it in units as INT-006 and INT-007 were. The ready queue below also
holds INT-008, INT-010, EXEC-001, APP-001, OPS-004, OPS-005 and QA-003; prefer the task that unblocks
the most downstream work, and if a task's toolchain cannot execute on this host (see "Environment
requirements"), record that and take the next one.

## Ready queue

`python3 scripts/validate_v81.py --ready` reports eight dependency-ready tasks.

| Task | Milestone | Note |
|---|---|---|
| INT-010 | M3 | intelligence evaluation harness (`python/intelligence/evaluation/`, `tests/evaluation/`); selected by `--next` |
| OPS-002 | M7 | audit, privacy, retention and user data controls (newly ready) |
| INT-008 | M3 | compaction epochs and the bounded conversation projection |
| INT-010 | M3 | intelligence evaluation harness (`python/intelligence/evaluation/`, `tests/evaluation/`) |
| EXEC-001 | M4 | Rust machine control and execution-target lifecycle |
| APP-001 | M5 | Rust server API/control composition and the walking skeleton |
| OPS-004 | M7 | usage, budget, quota and entitlement projections |
| OPS-005 | M7 | backup, restore and disaster-recovery consistency (`real_boundary: true`) |
| QA-003 | M8 | runtime concurrency, crash recovery and replay qualification (`real_boundary: true`) |

## Blocked work

### INT-011 — embedding pipeline and derived vector index (`BLOCKED_EXTERNAL`, implementation complete)
Reason: its dependency `INT-002` is `BLOCKED_EXTERNAL`, and the validator requires every dependency
to be `PASS` before a task may be `PASS`.
Exact unblock condition: set `QUANSIO_TEST_ANTHROPIC_API_KEY` and `QUANSIO_TEST_OPENAI_API_KEY` in an
environment with provider egress, run
`uv run --project python python -m pytest python/tests/intelligence/test_model_gateway_live.py -q`,
record that run as INT-002's `real_boundary_evidence`, and flip INT-002 (and INT-003) to `PASS`;
INT-011 then flips to `PASS` with the evidence already committed.
Independent work available: yes — everything in the ready queue.

### INT-007 — semantic memory with provenance (`BLOCKED_EXTERNAL`, implementation complete)
Reason: its dependency `INT-006` is `BLOCKED_EXTERNAL`, and the validator requires every dependency
to be `PASS` first. Both acceptance statements already hold: a restart succeeds with memory disabled
(proved structurally — the model may carry no recovery state and the store's statements name only
`memory_entries`), and a deletion stops future retrieval, verified in both planes including the window
before the derived index refreshes.
Exact unblock condition: the same provider credentials as INT-011 — set
`QUANSIO_TEST_ANTHROPIC_API_KEY` and `QUANSIO_TEST_OPENAI_API_KEY`, run INT-002's live suite, record
it and flip INT-002 (then INT-003, INT-011, INT-006 and INT-007) to `PASS`.

### INT-006 — Knowledge Fabric (`BLOCKED_EXTERNAL`, implementation complete)
Reason: its dependency `INT-011` is `BLOCKED_EXTERNAL`, and the validator requires every dependency
to be `PASS` before a task may be `PASS`. Both acceptance statements are already verified against
real PostgreSQL: entries are tenant-scoped and provenance-addressable, and removing a source
quarantines the knowledge derived from it and empties both the fabric's retrieval view and the
derived index in one operation (well inside DOSSIER.md §21.3's 60 s bound).
Exact unblock condition: the same provider credentials as INT-011 — set
`QUANSIO_TEST_ANTHROPIC_API_KEY` and `QUANSIO_TEST_OPENAI_API_KEY`, run INT-002's live suite, record
it and flip INT-002 (then INT-003, INT-011 and INT-006) to `PASS`.

### INT-002 — server-side model gateway (`BLOCKED_EXTERNAL`, implementation complete)
Reason: the live provider conformance suite cannot run here.
Exact unblock condition: `QUANSIO_TEST_ANTHROPIC_API_KEY` + `QUANSIO_TEST_OPENAI_API_KEY` (plus egress
to `api.anthropic.com` / `api.openai.com`), then the live suite and a recorded run. Everything else
about the task is implemented and verified offline (173 plane tests at the time, 6 skipped live cases
that name the missing variable).

### INT-003 — deterministic model selection, DLP and bounded failover (`BLOCKED_EXTERNAL`, implementation complete)
Reason and unblock: identical to INT-002, whose live suite is its provider path.

No other task is blocked. 27 tasks declare `real_boundary: true`; each is recorded the same way
rather than fabricated.

## Known defects and recorded limitations

- **Plan batches cannot yet reference their own nodes (RUN-003 limitation, still open).**
  `GraphChange::CreateWorkNode` generates a `wn_` id inside the transaction with no alias, so a plan
  that creates a parent and a child together cannot point the child at the new parent. *Fix (before
  the planner is driven from the turn loop in anger):* add an optional caller-supplied alias to
  `CreateWorkNode` in `crates/graph/src/transaction/`, resolve aliases to generated ids inside the
  batch, and widen RUN-003's planning tests. Verified still absent on `main` this run.
- **Runtime state tables are duplicated and guarded (RUN-001).** The runtime keeps its own copies of
  the DOMAIN run/turn/step/attempt status values because `crates/graph` depends on the server's schema
  module (a package cycle). `tests/architecture/test_runtime_state_parity.py` fails if either copy or
  the database `CHECK` constraints diverge. *Consolidation:* move the pure state enums into
  `crates/core`, re-export from both, then retire the parity test.
- **Cross-process bindings that belong to the composition root (APP-001):** INT-005's context bridge
  production RPC implementation, RUN-008's `GatewaySemanticVerifier` binding, INT-003's selector/DLP
  guard as the deployed default (an uninjected gateway keeps the INT-002 path), and the real
  `WorkGraphPort` wiring.
- **RUN-010:** the canonical prefix catalog has no budget prefix, so `BudgetService::create` takes the
  budget id from its creator instead of minting one.
- **RUN-008:** a run does not yet record its model route, so the Rust verifier passes
  `claimant_model=None` and a contract requiring `independent_model` is refused rather than
  self-certified.
- **INT-012's pinned injection corpus** (`tests/security/injection/corpus.json`) is what QA-007
  consumes.
- No other known defects: the intermittent intelligence-plane failure (the route-id clock dependency)
  remains fixed with its regression test, and the determinism audit found no further leaks.

## Tests

Last run this session, at `340dbce83e36` (INT-007 unit 4 evidence bundle
`evidence/INT-007/2026-09-13T06-38-37Z/`; INT-006's is `evidence/INT-006/2026-09-13T06-14-00Z/` and
INT-011's `evidence/INT-011/2026-09-13T04-36-53Z/`):

- `QUANSIO_TEST_POSTGRES_URL=... (cd python && uv run --frozen pytest -q)` → **448 passed, 6 skipped**
  (the 6 are the live-provider cases that need `QUANSIO_TEST_*` credentials)
- `QUANSIO_TEST_POSTGRES_URL=... (cd python && uv run --frozen pytest tests/integration -q)` → 72 passed
- `(cd python && uv run --frozen pytest tests/intelligence/test_memory_models.py tests/intelligence/test_memory_store_boundaries.py tests/intelligence/test_memory_candidates.py tests/intelligence/test_memory_retrieval.py -q)` → 46 passed
- `QUANSIO_TEST_POSTGRES_URL=... (cd python && uv run --frozen pytest tests/integration/test_memory_store.py tests/integration/test_memory_proposals.py tests/integration/test_memory_semantic_channel.py -q)` → 31 passed
- `(cd python && uv run --frozen pytest tests/intelligence/test_memory_models.py tests/intelligence/test_memory_store_boundaries.py -q)` → 22 passed
- `QUANSIO_TEST_POSTGRES_URL=... (cd python && uv run --frozen pytest tests/integration/test_memory_store.py -q)` → 18 passed
- `QUANSIO_TEST_POSTGRES_URL=... (cd python && uv run --frozen pytest tests/integration/test_knowledge_store.py -q)` → 24 passed
- `QUANSIO_TEST_POSTGRES_URL=... (cd python && uv run --frozen pytest tests/integration/test_knowledge_forgetting.py -q)` → 4 passed
- `QUANSIO_TEST_POSTGRES_URL=... (cd python && uv run --frozen pytest tests/integration/test_knowledge_semantic_channel.py -q)` → 4 passed
- `(cd python && uv run --frozen pytest tests/intelligence/test_knowledge_models.py tests/intelligence/test_knowledge_store_boundaries.py tests/intelligence/test_knowledge_ingestion.py tests/intelligence/test_knowledge_indexing.py -q)` → 51 passed
- `(cd python && uv run --frozen pytest tests/intelligence/test_embed_rpc.py -q)` → 10 passed
- `(cd python && uv run --frozen pytest tests/intelligence/test_embeddings.py tests/intelligence/test_embedding_sources.py -q)` → 30 passed
- `(cd python && uv run --frozen pytest tests/intelligence/test_server_launch_embed.py -q)` → 2 passed
- `(cd python && ruff check . && ruff format --check . && mypy intelligence)` → clean
- `uv run --project python pytest tests/architecture tests/contract --deselect tests/contract/test_contracts.py::test_generated_bindings_are_current -q` → 130 passed
- The baseline gates that do not execute a newly linked binary (validate_v81, dossier-consistency,
  arch_check, authority-pointers, workspace, legacy-map, inventory, contract-lint-compat,
  supply-chain) → all CLEAN
- Contract drift (`scripts/ci/gen_contracts.py` `check([...])`, run around the host incident below) →
  `DRIFT PROBLEMS: []`

Failing: none. Still required: `bash scripts/ci/ci.sh` (blocked this session by the host incident —
see "Environment requirements") and the per-task tests of the remaining registry tasks.

## Migrations / state changes

- No new migration this run. `migrations/0008_embedding_index_key.sql` remains the latest schema
  change; the derived table is unchanged by the wire work.
- `config/models.yaml` gained `embedding_dimensions` on the embedding route and lost the `/v1`
  segment from the `openai` base_url; `schemas/json/models-config.schema.json` gained the property.
  A model that declares the `embeddings` capability must now declare its width, and the catalog
  loader refuses one that does not.
- `registries/progress.json`: INT-011 `IN_PROGRESS` → `BLOCKED_EXTERNAL` with
  `implementation_complete: true` and its evidence bundle; `TASKS.md`, `registries/task-graph.json`
  and `MANIFEST.json` regenerated with `--write`.

## Working tree

Modified: none outstanding — every change of this run is committed on `main`
(`[INT-011] merge the embedding wire, the authoritative rebuild and the deletion seam`).
Untracked: none expected. A progress/evidence commit follows this handoff.
Generated (never hand-edit): `TASKS.md`, `registries/task-graph.json`, `MANIFEST.json` —
regenerate with `python3 scripts/validate_v81.py --write`. Contract bindings are generated by
GOV-004 tooling; regenerate, never hand-edit.
Do not overwrite: the V8.1 authority set (`MANIFEST.json` lists digests).
Do not trust: `target/debug/deps/schema_bootstrap-d021e59da74aaafb` — it was overwritten on purpose
to host the contract generator during the host incident below. It was deleted afterwards; cargo
relinks it.

## Commands

Prepare a database for the Python integration suites:
`uv run --project python python scripts/dev/seed_test_database.py --admin-url <dsn> --database <name> --tenant <tn_id>`
Build: `bash scripts/dev/bootstrap.sh` (or `cargo check --workspace`, `pnpm build`, `(cd python && uv sync --frozen)`)
Test: `uv run --project python pytest tests -q` · `(cd python && uv run --frozen pytest -q)` · `pnpm test` · `cargo test --workspace`
Validate: `python3 scripts/validate_v81.py` (regenerate views with `--write`)
Run locally: `python -m intelligence.server --transport tcp --host 127.0.0.1 --port 50065` from
`python/` (the launcher, exercised by `python/tests/intelligence/test_server_launch_embed.py`)
Qualification: `scripts/ci/inventory.py --scan`, `check_authority.py --check`, `workspace_check.py`,
`scripts/ci/arch_check.py`

## Environment requirements

Services: the repository's own dev stack starts with `scripts/dev/up` (idempotent; it re-reads
`config/dev.yaml` for images/ports/bucket while preserving credentials and the MinIO SSE key in the
gitignored `.env`). Docker Compose project `quansio-dev`: Postgres 17 + pgvector on 55440, NATS
JetStream on 54230/54231, MinIO on 59010/59011, stub model provider on 59020, optional Qdrant on
59030. Dev-only credentials live in the gitignored `.env`; the documented dev defaults are
`quansio` / `quansio-dev-only` for Postgres and `quansio-dev` / `quansio-dev-only` for MinIO.
`scripts/dev/_common.sh` puts Docker Desktop's credential helper on `PATH` before any pull.

Verified healthy this session: `docker info` (29.7.2), Postgres 55440, MinIO 59010 (150 objects under
`tenants/`), stub provider 59020. The dev-test stack (`quansio-dev-test`, offset ports 56440/59110)
was also running for another agent; leave it alone.

Server signing key: approvals are signed with `QUANSIO_APPROVAL_SIGNING_KEY` (HMAC-SHA256 over the
bound receipt fields). Tests set it explicitly; an absent or empty key fails closed.

Credentials/handles: no production `QUANSIO_TEST_*` credentials are set; real-boundary tasks must
record `BLOCKED_EXTERNAL` until provided. Database-backed tests read `QUANSIO_TEST_POSTGRES_URL`, for
example `postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio` (the baseline pipeline derives
that URL from the generated `.env` automatically). Never place raw secrets in this file.

**Host incident (ACTIVE at the end of this session).** The host is again refusing to execute newly
created binaries: a byte-identical `cp` of a working test binary hangs in `_dyld_start` while the
original runs (captured in the INT-011 evidence bundle). Consequences: `cargo test` cannot run (its
freshly linked test binaries hang), `cargo run` for `scripts/dev/contract-gen` cannot run, and
therefore `bash scripts/ci/ci.sh` cannot complete (`toolchains` and `contract-drift` are the two
affected gates). `sudo` is not available, so neither a `syspolicyd` kickstart nor a reboot can be
performed from here. Workaround that does work: write the fresh binary's bytes into a **pre-existing**
inode (`cat <new-binary> > <old-binary-path>`, `chmod +x`) and run that path — this is how the
contract-drift gate was verified (zero drift). Restrict candidates to extension-less test binaries
under `target/debug/deps` and delete the host path afterwards so cargo relinks it. *Unblock:* reboot
the host, or wait for the incident to clear as it did earlier in this mission, then run
`bash scripts/ci/ci.sh`. Note the workspace `target/` directory was also emptied at some point for
disk space, so the first `cargo test` after the incident clears will recompile the whole workspace.

Disk caveat: the volume hosting this work holds ~104 GiB free now but has been nearly full earlier
in the mission. Keep at most two concurrent Rust worktrees, delete a finished worktree's `target/`
after its task is closed, and `rm -rf target/debug/incremental` when space is needed.

Concurrency caveat: several agents may run in parallel worktrees against this ONE shared dev stack
(compose project `quansio-dev`). Never run `scripts/dev/down`, never delete its volumes, and never
recreate its containers from a worktree-modified compose file. The dev-stack integration suite
exercises its own compose project (`quansio-dev-test`) on offset ports.

External dependencies: Rust 1.97.1 (+ rustfmt/clippy), Node 26 + pnpm 11.8, Python 3.12 + uv 0.12,
protoc 36, Swift 6.3, Docker 29. All present. `cargo-deny` is absent, which the supply-chain gate
reports as an informational result rather than a failure.

## Architecture decisions made during implementation

- **Embedding routes resolve by request class, not by `routing.primary` (INT-011).** The gateway's
  default conversation selector is class-blind (it returns `routing.primary`), which would send an
  embedding call to a chat model with no embeddings endpoint, so `ModelGateway` holds a second,
  class-driven selector instance for the embedding class. Its failover candidates are filtered to
  models declaring both the `embeddings` capability and a width, because INT-003's selector does not
  eligibility-check the tail of `fallbacks`.
- **The catalog declares the embedding width (INT-011).** `embedding_dimensions` is required for a
  model that declares the `embeddings` capability: without a declared width a route cannot be checked
  against the width `derived.embeddings` pins, and failover must never cross embedding spaces.
- **The intelligence plane reads object storage but never writes it (INT-011).** `S3ObjectReader`
  exposes list and get only. Artifact bytes are the Rust artifact authority's to write, so there is no
  `put`/`delete` surface for a product path to misuse.
- **Corruption is refused; unindexable content is skipped and recorded (INT-011).** A missing object or
  a digest mismatch aborts the read (the index must not stand in for content nobody can verify); a
  binary, empty or oversized artifact is skipped with the rule recorded, because a real tenant holds
  attachments and media and a rebuild's coverage must be auditable.
- **Plan batches cannot yet reference their own nodes (RUN-003 limitation)** — see "Known defects".
- **Runtime state tables are duplicated and guarded (RUN-001)** — see "Known defects".
- **Tool declarations are control-plane data, not source constants (RUN-011).** Every tool is declared
  in `config/tools.yaml` and validated against the generated catalog `schemas/catalog/tools.yaml`.
- **The tool schema vocabulary is closed (RUN-011).** `crates/tools` implements exactly the JSON
  Schema keywords it documents and rejects a declaration using anything else; every object schema must
  set `additionalProperties: false`. Widening the vocabulary is a code change with a test.
- **Parallel tool dispatch adds no dependency (RUN-011).** `runtime::turn_loop::parallel::join_all`
  polls boxed futures on the task that already drives the turn; `plan_rounds` gives control-plane
  tools their own round and defers a repeated (tool, canonical-args) pair.
- **A parked call is resumed, never re-proposed (RUN-011).** The dispatch records the reserved effect
  and its dispatch token in ProtocolState; an unsettled effect is never re-dispatched.
- **Proposal trust fails closed (RUN-011).** Policy evaluates a proposal's causal chain as
  `UNTRUSTED_EXTERNAL` until INT-005's ContextProjection supplies real segment labels through the
  `ProposalTrustSource` seam.
- Reconciliation, authority pointers and workspace mapping are executable gates
  (`scripts/ci/inventory.py`, `check_authority.py`, `workspace_check.py`, `arch_check.py`), not prose:
  governance that cannot fail a build is not governance.
- Root-level architecture and contract tests run in the intelligence-plane environment
  (`uv run --project python pytest tests -q`) because `config/*.yaml` validation needs PyYAML.
- Repo-level tooling, caches, build output and Swift `/.build` are gitignored; `Cargo.lock`,
  `pnpm-lock.yaml` and `python/uv.lock` are committed for deterministic builds (DOSSIER.md §18).

## Resume instructions

1. Read `AGENTS.md`.
2. Read `DOSSIER.md` (and the `DOMAIN.md` sections named by the selected task; §0 glossary always).
3. Read this `HANDOFF.md`.
4. `git status` / `git log --oneline -5`; confirm `main` is clean and `python3 scripts/validate_v81.py`
   prints `V8.1 VALIDATION: PASS`.
5. `python3 scripts/validate_v81.py --ready` and `--next`; take the selected task, or the ready task
   that unblocks the most downstream work.
6. Reconcile the selected task against the repository before editing (AGENTS.md RECONCILE), claim it,
   and continue. Do not stop unless manually stopped; if a task's toolchain cannot execute on this
   host, record the blocker and take the next ready task.
