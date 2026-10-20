# APP-007 — the live surface, completed (with the EXEC-002, RUN-006, APP-010 and INT-002 changes it carried)

Closes the gaps recorded in the previous APP-007 notes ("Not yet:" in DOMAIN.md §8.4.2), and adds the pieces a person needs to use a worker and keep an agent on a short leash.

| Gap | Owner | Change | Proof |
|---|---|---|---|
| Drag, right click, middle click, double click, modifier chords | `qworkerd/src/browser/actions.rs`, `machine/src/gateway/wire.rs`, `server/src/api/live_sessions.rs`, desktop `live/session.ts`, `renderer/Live.tsx` | `UserClick {button, click_count, modifiers}`, `UserDrag`, `user_key_allowed` (named keys with modifiers, letter/digit with Alt/Control/Meta) | `worker_live.rs` real Chrome; `wire.rs` unit; `live-input.test.ts` |
| `target:` channels open to any tenant member | `server/src/api/stream.rs` | `target_admitted`: the workspace's readers only, WebSocket and SSE | `only_a_reader_of_the_targets_workspace_may_watch_its_channel` |
| Decisions to an absent worker dropped | `server/src/worker/hub.rs`, `machine/.../browser_guard.rs` | replay recorded decisions on attach, oldest first | `a_decision_made_while_the_worker_was_away_is_told_to_it_when_it_attaches` |
| Starting a worker needed flags and a seeded database | `server/src/api/worker_targets.rs`, desktop `main/worker-launcher.ts`, `renderer/WorkerPanel.tsx` | `ProvisionTarget` for `customer_private_worker`; *Start a worker…* in the Live surface | `worker_live.rs` (roles, idempotence, concurrency, attach); `worker-launcher.test.ts`; `worker-launcher.e2e.test.ts` (real server, real daemon) |
| No per-action policy mode, no read-only posture | `server/src/policy/autonomy.rs`, `evaluation.rs`, `identity/settings.rs`, `api/routines.rs`, `api/commands.rs` | `autonomy_mode` (four modes, strictest layer wins); routine `mode: observe` narrows the run's projection to reads | unit; `turn_loop.rs`; `routines.rs`; settings tests |
| Pictures never reached a model | `python/intelligence/model_gateway`, `context/turn.py` | optional image parts, OpenAI/Anthropic carriage, fixed token cost | `test_model_gateway_pictures.py` (the server side of the pictures is a later change; see notes) |

State touched: none new (no migration). Settings keys: `autonomy_mode`. Routine template key: `mode`. Commands: `ProvisionTarget` (already in the catalog, now served for one substrate), `SendBrowserInput` kinds `drag` and optional fields. Events: none new. External effects: none. Real boundaries: PostgreSQL, a real Chrome, a real `qworkerd`, a real `quansio-server`.

Not done in this change (recorded, not hidden): the pictures are not yet produced by the worker or attached by the server (the Python half is in); frames are still base64 in JSON; the desktop viewer has not been driven in a real Electron window.
