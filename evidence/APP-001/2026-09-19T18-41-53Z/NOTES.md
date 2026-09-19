# APP-001 — regression: events committed after a subscribe were never delivered

**Failing test / what exposed it.** APP-003's reconnect E2E (2026-09-19): a client that
subscribed to `/v1/stream` before any event existed received nothing afterwards. The new
`websocket_delivers_events_committed_after_subscribe` in
`crates/server/tests/walking_skeleton.rs` reproduces it (red before the fix, timing out at
the delivery wait).

**Cause.** `api/stream.rs` bound the live-frame sender as `_live`, keeping it alive for
the handler's whole scope. `StreamSession::next_frame` waits while either channel is open,
so after the initial replay ended it waited on the live channel forever and `tail_events`
was never reached. The existing skeleton test posted before subscribing, and its "later
frame" assertion was met by the first run's own later events.

**Fix** (`fix/APP-001-stream-tail`, merged 0943011). The sender is dropped at once; nothing
on this path produces live frames yet, and live frames are transient by contract
(DOMAIN.md §9.3) while durable events are not optional. Recorded per the regression
protocol with no transient FAIL: fix branch → merge → this bundle.

**Also in this period.** `quansio-dev-seed` (`src/bin/quansio-dev-seed.rs`), a dev-only
binary in this crate: creates/drops a scratch database, applies the schema, seeds a tenant
for end-to-end harnesses, so no client package ever writes an authority table (305b3f8).

**Verification.** `cargo test -p quansio-server --test walking_skeleton` — 3 passed;
`--test api` — 12 passed; fmt/clippy clean; `bash scripts/ci/ci.sh` PASS at 305b3f8.
