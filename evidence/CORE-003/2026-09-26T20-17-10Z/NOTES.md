# CORE-003 — a replay longer than the buffer waits for the subscriber

Found by QA-009 (P1): every WebSocket subscriber without a recent cursor on a workspace with more
than 64 events was disconnected after 64 frames, within 40 ms, however fast it read
(`served-replay.log`: the release build of `main` stops at frame 65 — the 64 events and the
`STREAM_BACKPRESSURE` error frame — on a workspace of 300 threads). The replay reader
(`crates/events/src/stream.rs`, `pump_events`) read batches of 256 events and pushed each frame into
the 64-frame subscriber buffer with `try_send`, recording backpressure the moment it was full — so a
full buffer, which a replay longer than the buffer always produces, was treated as a slow consumer. A
desktop reconnecting after a gap, or opening for the first time on a busy workspace, was cut off
repeatedly.

Fix: the reader awaits room in the buffer, and disconnects with `STREAM_BACKPRESSURE` only when the
subscriber takes nothing for `StreamConfig::stall_timeout` (default 5 s); memory stays bounded by the
buffer either way. DOMAIN.md §9.3 now states the rule.

Tests (`tests.log`): `a_consumer_that_keeps_reading_replays_far_past_the_buffer` — 300 events through
a buffer of 2 to a subscriber slower than the reader (1 ms per frame) but never stalled, all delivered;
the existing `a_slow_consumer_is_disconnected_without_blocking_the_publisher` (a subscriber that never
reads is still disconnected, with exactly the buffer's frames delivered, and the publisher is not
blocked); the whole `quansio-events` suite; fmt and clippy (`lint.log`). Mutation (`mutation.log`):
disconnecting at once on a full buffer (no stall window) fails the long-replay test. Served
(`served-replay.log`, `served_replay_check.py`): the same 300-thread workspace replays all 600 frames
and then receives live events on this branch's release build.
