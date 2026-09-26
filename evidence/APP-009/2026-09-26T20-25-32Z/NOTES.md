# APP-009 — a thread list decides access in one query

Found by QA-009 against DOSSIER §21.2 ("API read ≤ 200 ms p95"): every read the desktop and web make
answered in under 20 ms p95 except `GET /v1/threads`, at p50 397 ms / p95 464 ms for a workspace of
about 200 threads with eight clients reading — enough to put the whole read mix over budget. The list
handler (`crates/server/src/api/collaboration.rs`) called `thread_access` once per thread, each its own
transaction and query, to decide whether a protected thread may be listed for this reader.

Fix: `thread_access_many` (`crates/server/src/control/collaboration/access.rs`) decides access for the
whole list in one query with the same rules (objective threads and threads without participants are
open; a protected thread admits active participants with an active workspace membership; a thread
absent from the answer is missing), and the list uses it.

Tests (`tests.log`): `a_thread_list_decides_access_exactly_as_one_thread_does` — a protected group
thread, a second with other members, an open objective thread and a missing id, for the owner, a
participant, a member of the other thread, an outsider and an anonymous reader: the batched decision
equals `thread_access` for every pair; the collaboration, conversation surface and API suites; fmt and
clippy (`lint.log`). Mutation (`mutation.log`): a batched decision that admits every thread fails the
equality. Served (`served-thread-list.log`): 200 threads, one reader, release builds — p50 175 ms / p95
201 ms before, 5.5 ms / 10 ms after (host load 14; QA-009 re-measures the read mix).
