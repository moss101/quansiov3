# APP-009 / RUN-002 — concurrent messages to one teammate

**Reported.** `apps/desktop/tests/collaboration.e2e.test.ts` ("keeps one order for two clients posting at once…") passed alone
but failed once in a full parallel run with `INTERNAL: illegal agent_thread transition: ACTIVE -> ACTIVE`.

## What was actually wrong
A teammate's row is committed by `TeammateStore::create` *before* its AgentThread exists (`create` then calls
`provision_thread`). A second message that arrives in that window finds the teammate and also calls `provision_thread`.
Provisioning was "look for a thread, create one, then a **bare** `activate`":

1. **The reported error.** Caller A creates the thread; caller B finds it still `PROVISIONED` and activates it through
   `ensure_active`; A's bare `activate` then fails `ACTIVE -> ACTIVE`, which surfaced to the person who posted as `INTERNAL`.
   (`ensure_active` was an earlier fix for the same symptom on the *post* path; `provision_thread`'s own activation had been
   missed.)
2. **A second defect behind it, worse than the error.** Nothing made the thread unique per teammate (`definition_id` has no
   unique index and `create_thread` always inserts), so callers that all saw "no thread" each **created their own**. The
   reproduction below shows eight concurrent callers handed eight different threads for one teammate: one teammate with several
   threads, its messages split between them. Fixing only the error would have hidden the symptom and kept this.

## Fix
`AgentStore::ensure_thread_for_definition` (in the owner of `agent_threads`): a plain read when the thread exists; otherwise a
transaction that takes a transaction-scoped advisory lock for `(tenant, definition)`, looks again, and either creates the thread
and its `agent.thread_provisioned` event or — if another caller got there first — abandons the transaction (nothing written, no
event; the event store rightly refuses a commit with none) and returns the winner's thread via `StateConflict`.
`provision_thread` now always does that and then `ensure_active`, so its contract ("the teammate's ACTIVE thread") holds even
when it found a `PROVISIONED` one. `create_thread` shares the event emission, behaviour unchanged.

No migration. A unique index would make the database enforce this, but it would fail on any database that already holds
duplicate threads from this bug, so it is not forward-safe; the lock serializes creators in every server process.

## Proof
* `crates/server/tests/teammate_thread_race.rs`: 12 rounds × 8 simultaneous callers on a teammate with no thread, against real
  PostgreSQL. **Before the fix it fails in round 0** (8 callers, 8 threads); after, every caller succeeds, gets the same `ACTIVE`
  thread, and exactly one thread row exists. Stable across repeated runs.
* Mutants, all killed (every one compiles; the restored tree is byte-identical to the fix): no lock → 4 threads; a bare activate
  again → **`illegal agent_thread transition: ACTIVE -> ACTIVE`, the reported message**; a creator that lost the race is told it
  failed → `state conflict`; look-then-create → 8 threads.
* The whole desktop suite, run three times under parallel load, and all Rust.

## Mistake made along the way (so it is not repeated)
My mutation script backed files up by `basename`, and the two files it mutated are both called `store.rs`; the second backup
overwrote the first and the "restore" copied the teammates store over the agent store, discarding my uncommitted edits to it.
I caught it from the control run failing and a diff of ~2,500 deletions, restored the file from git, reapplied the edits and
**committed immediately**. The script now backs up by full path and checks the tree equals the committed fix after restoring.

## Not fixed here (found, separate)
* **Two default teammates.** When a workspace has no default teammate, two simultaneous first messages each create a
  "General assistant" teammate (`default_for`, then `create`, with no lock). That is a different race with a different
  symptom (duplicate teammates, no error); it needs a decision about what makes a default unique.
* Existing databases may already hold duplicate threads from this bug. They are not merged or deleted by this change;
  `thread_of` / the new lookup both pick the oldest.
