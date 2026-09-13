# INT-008 reconciliation — compaction epochs and the bounded conversation projection

Recorded 2026-09-13 before editing, per AGENTS.md RECONCILE.

## Canonical owner
`crates/server/src/runtime/compaction/` (Rust) and `python/intelligence/context/compaction/` (Python) —
the two paths the task names. Neither exists today: the runtime modules are `agents`, `budgets`,
`checkpoints`, `context_bridge`, `orchestration`, `planning`, `protocol_state`, `recovery`,
`state_machine`, `turn_loop` and `verification`, and the Python context package has no `compaction`
subpackage. Coverage: `GENUINE_GAP`.

## What already exists and is reused rather than re-created
- **The persistence shape (CORE-001, PASS).** `migrations/0001_canonical_schema.sql` declares
  `public.compaction_epochs`: id `cep_`, tenant_id, thread_id, run_id, `seq`, `source_from_sequence`,
  `source_to_sequence`, `summary_artifact_id`, `token_estimate`, status
  `pending|installed|rejected_stale`, `created_by_model_route_id`, timestamps, `UNIQUE (thread_id, seq)`
  and `CHECK (source_to_sequence >= source_from_sequence)`. The schema already says what an epoch *is*: a
  versioned range over a thread's event sequence, with a state for a stale one. No migration is needed.
- **The event sequence (CORE-003, PASS).** `crates/events` assigns a per-tenant sequence inside the
  same transaction that writes an event, gap-free, so `source_from_sequence`/`source_to_sequence` name a
  real, ordered range rather than an approximation.
- **Protocol truth (CORE-006, PASS).** `runtime/protocol_state` owns `ProtocolState`, `Wait`,
  `PendingToolCall`, `PendingModelCall`, browser/terminal holders and the pure `next_safe_action`. That is
  what the task means by "keep tool/protocol state outside summaries": the state a resumed run obeys
  lives there, keyed by the run, and a summary never becomes a place protocol truth is read from.
- **Projections (CORE-009, PASS).** `crates/events/src/projection.rs` holds deterministic rebuildable read
  models with checkpoints; the summary a compaction epoch references is an artifact
  (`summary_artifact_id`, CORE-007), and the derived structures are rebuildable rather than authoritative.
- **The context projection (INT-005, PASS).** `crates/server/src/runtime/context_bridge/` validates a
  `ContextProjection` before a run sees it, and `python/intelligence/context/projection.py` assembles one
  with a token ledger. A bounded conversation projection is what a compacted thread is *read through*, so
  the Python half belongs beside that module rather than in a new plane.
- **The route id an epoch records (INT-002/INT-003).** `created_by_model_route_id` names the route that
  produced the summary, so an epoch is attributable to the model that wrote it.

## The two acceptance statements, and what they require
1. **Fork/revert never installs compaction from abandoned history.** An epoch is created against a source
   range and may be installed only while the run's position still contains that range. A fork or a revert
   moves the position off it, so installation must be refused — the schema's `rejected_stale` state is
   where that refusal is recorded, and the decision has to be made inside the transaction that would
   install it, not read and then written.
2. **Exact protocol replay does not depend on summary text.** Replay reads the event log and the protocol
   state. A summary is an *optimisation for the model's context*, never an input to replay, so this is a
   property to enforce structurally: the replay path must not read `compaction_epochs` at all. The
   reconciliation's own check is that the recovery read set (RUN-009's `RECOVERY_READ_TABLES`) stays
   exactly what it is today — adding a compaction table to it would be the defect this statement names.

## Persistent state touched
`public.compaction_epochs` (rows, new writes only). No new table, no schema change, and no change to any
existing table.

## Contracts / events
No `DOMAIN.md` or proto change is needed: §5.7's protocol state and §9's event log already carry the
authority this task reads, and `compaction` is already a `RuntimeEvent` family
(`EVENT_FAMILY_COMPACTION`). Whether an epoch's lifecycle emits `compaction.*` events is a question for
the implementation to answer against that family rather than invent a vocabulary for.

## External effects, capabilities, approvals
None of its own. Producing a summary is a model call, so it goes through the one gateway under the same
policy, DLP and approval rules as any other call; writing the epoch is a runtime state transition.

## Real boundaries and environment
`real_boundary: false`. The Rust half needs `cargo test` and the PostgreSQL dev stack
(`QUANSIO_TEST_POSTGRES_URL`); the Python half needs neither. **Environment note:** the host incident that
had been blocking execution of newly created binaries for this whole session has cleared — a freshly
compiled probe now runs — so `cargo test` and the full `bash scripts/ci/ci.sh` baseline are runnable
again, and this task's Rust half is verifiable on the host rather than blocked.

## Rollback / recovery
An installed epoch is a row with a source range; reverting the thread's position makes it stale rather
than wrong, and because replay never reads it, a defective epoch cannot corrupt recovery. Deleting the
rows loses an optimisation, not a fact.

## Plan of units
1. **The Rust epoch lifecycle** (`crates/server/src/runtime/compaction/`): create an epoch for a source
   range, install it only while the position still contains that range, and record `rejected_stale`
   otherwise — one transaction per transition, with the staleness decided against durable state.
2. **The Rust tests** for both acceptance statements: a fork/revert refuses to install from abandoned
   history, a stale install writes `rejected_stale` and leaves the position untouched, and the replay read
   set still names no compaction table.
3. **The Python half** (`python/intelligence/context/compaction/`): the bounded conversation projection —
   what a summary may contain, the token bound a projection is assembled under, and the rule that tool and
   protocol state are never summarised — with tests that fail if a summary is asked to carry them.
4. **The task's evidence panel**, and the running baseline (`bash scripts/ci/ci.sh`) recorded as the
   session's first complete pipeline run since the host incident cleared.
