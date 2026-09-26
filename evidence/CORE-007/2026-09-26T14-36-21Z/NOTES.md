# CORE-007 — artifact store failures reported as INTERNAL

Found by QA-008 (finding U-9): while the dev database restarted, creating an artifact answered
`VALIDATION_BOUNDS` with the raw driver message ("artifact store database: expected to read 5 bytes…").
`artifact_error` in `crates/server/src/api/artifacts.rs` sent every store variant it did not name to
`VALIDATION_BOUNDS` — the code that tells a client its own input was wrong, so a client would not retry
and the person saw a database internal.

Now: `InvalidValue` stays `VALIDATION_BOUNDS`; `NotFound`, `Deleted` and `NoBytes` are `NOT_FOUND`;
`AccessDenied` stays `CAPABILITY_DENIED`; `Database`, `Schema`, `ObjectStore`, `Decode` and
`DigestMismatch` are `INTERNAL` with a fixed sentence, the detail written to the server log. The match is
exhaustive, so a new variant must be classified.

Tests: two unit tests (`unit.log`); the artifact, workspace, workflow and API integration suites
(`integration.log`, against the dev database and MinIO); fmt and clippy (`lint.log`). Mutation: restoring
the old mapping (store failure → `VALIDATION_BOUNDS` with the raw message) fails the store-failure test
(`mutation.log`, exit 101).
