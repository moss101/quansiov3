# CORE-009 regression fix — an open stream outlived its session

Found by QA-008 (2026-09-26): after `RevokeUserSessions`, the revoked person's desktop still read
"Connected." and kept receiving the workspace's live events. `/v1/stream` (WebSocket) and
`/v1/stream/events` (SSE) called `TenantScope::resolve` once, on connect; nothing re-checked the
credential while the connection stayed open, so revocation, sign-out, member removal or suspension,
and access-token expiry did not end an open stream.

Fix (`crates/server/src/api/stream.rs`): both loops keep the opening headers and re-resolve them every
`REVALIDATE_EVERY` (2 s); a credential that no longer resolves — or resolves to another tenant — ends
the stream with the typed refusal as the last frame (`AUTH_INVALID_TOKEN`, `AUTH_EXPIRED`,
`SCOPE_FORBIDDEN`, `TENANT_MISMATCH`). A client with a refreshable session reconnects with its new
token and its cursor, losing nothing durable.

Test: `crates/server/tests/identity.rs::an_open_stream_closes_when_its_session_is_revoked` — a live
session's stream survives a re-check (positive control); signing out closes an open stream with
`AUTH_INVALID_TOKEN` within one re-check; the revoked token cannot open a new one. Mutation: disabling
the re-check fails the test.
