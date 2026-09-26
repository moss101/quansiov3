# INT-004 — search catches its index up instead of rebuilding it

Found by QA-009 against DOSSIER §21.2 ("index query ≤ 300 ms p95"): steady searches answered in
18 ms p95, but the first search after any artifact change took p50 385 ms over 120 small notes, and
the cost grows with the corpus. `SearchService::refresh` (`crates/server/src/api/search.rs`) compared a
corpus fingerprint and, on any change, rebuilt the tenant's whole index — reading every artifact
version's bytes back from object storage — although `CreateArtifact` had already added the new
version to the index incrementally.

Fix: the refresh remembers what it last saw of the corpus (version count, newest version id, deleted
count). When only new versions arrived, it loads just the versions newer than the last one it indexed
(`SourceScope::after`, a `v.id > $3` bound in the indexer's Postgres source — version ids are
time-ordered) and upserts them. It rebuilds the whole index when the tenant was never indexed in this
process, when an artifact was deleted, or when the new versions it found do not account for the change
(for example ids not in commit order), so nothing is ever missed.

Tests (`tests.log`): `search_indexes_only_what_arrived_since_it_last_looked` — over MinIO behind a
counting object store, the first search reads the whole corpus, a search after one more upload reads
exactly one object and finds it, and a search with nothing new reads nothing; the indexer suite, the
artifact workspace and knowledge surface suites; fmt and clippy (`lint.log`). Mutation
(`mutation.log`): rebuilding on every change fails the one-read assertion. Served (`served-search.log`,
release builds, 120 notes): the first query after each of 20 writes, p50 236 ms / p95 262 ms before,
12 ms / 15 ms after.
