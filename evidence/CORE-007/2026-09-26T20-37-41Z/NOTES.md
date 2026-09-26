# CORE-007 — artifact changes are RuntimeEvents

Found by QA-009 (P1) while measuring usage accuracy on the served build: an uploaded artifact left no
event at all. The artifact store (`crates/server/src/artifacts/metadata.rs`) wrote `artifacts`,
`artifact_versions` and `artifact_grants` rows, but never the `artifact.*` events DOMAIN.md §9.2 names
(`created`, `version_added`, `grant_changed`). Everything downstream of the event stream was blind to
artifacts: OPS-004's usage projection never metered storage (`storage_bytes` stayed 0 whatever was
uploaded), a routine triggered by `artifact.created` never fired, an artifact webhook never delivered,
and no stream subscriber ever saw an artifact appear. `served-storage.log`: on the release build of
`main`, an 1,800-byte upload produced no event and metered 0 bytes.

Fix: the store writes `artifact.created` (kind, title, role), `artifact.version_added` (version id,
seq, `size_bytes`, media type, digest, producing run) and `artifact.grant_changed` (principal, level) in
the caller's transaction, so each change and its event commit or roll back together; a repeated upload
of the same bytes writes nothing. The events crate gains `append_in(conn, tenant, draft)` — append one
event on a caller's open transaction, sequence and outbox row included, no commit — which the store
uses (its methods compose into the caller's transaction by design); `commit_staged` is now built on it.
`ArtifactError::Event` is reported as `INTERNAL`.

Tests (`tests.log`): `artifact_writes_are_runtime_events_committed_with_them` — create, version, a
repeated upload and a grant in one transaction give exactly `created`, `version_added` (size 5) and
`grant_changed`; a rolled-back creation leaves no event; the `quansio-events` suite (the commit path now
runs through `append_in`); the artifact, artifact workspace and workflow, API, usage store, routines and
webhook suites (MinIO and the dev database); fmt and clippy (`lint.log`). Mutation (`mutation.log`): a
new version recorded under another event type fails the test. Served (`served-storage.log`): this
branch's release build writes both events and meters exactly the 1,800 bytes.
