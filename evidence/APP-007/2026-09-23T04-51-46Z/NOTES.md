# APP-007 — live browser/computer surface

## What was built (task branch `task/APP-007-live-surface`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
`apps/desktop/src/live/session.ts` flipped two fields of an in-memory object and nothing
reached a server. Ownership decisions are in `evidence/APP-007/RECONCILIATION.md`.

### Server (behind the surface; existing owners, no new store)

- `crates/server/src/api/live_sessions.rs`: `GET /v1/targets?workspace_id=` (targets with
  their browser sessions, terminal sessions and computer-control row) and
  `GET /v1/browser-sessions/{id}` (the session row, its run with the ProtocolState's
  `browser_control`, the target's terminals with durable cursors, the computer-control
  row, and `automation {paused, reason: user_control|policy|closed, agent_input_fenced}`).
  Both were catalogued in DOMAIN.md §14 and unserved.
- `RequestTakeover` / `Handback`, each one `commit_mutation_tx`: the machine authority
  moves the row (`BrowserSessionStore::request_takeover|handback` — the same row, never a
  second session; or `ComputerControlStore` request → complete / handback with its
  generation fence), the runtime parks or releases the owning run
  (`RuntimeStore::park_for_takeover_in` / `release_takeover_in`: ProtocolState
  `browser_control {holder: user}` + `RUNNING → WAITING_TAKEOVER`, and back), one
  `browser.control_changed` (or `target.control_changed`) event, one AuditEntry.
  Author-work role required; a handback of what the user does not hold, a takeover of a
  non-active session, and a second takeover are `CONFLICT_STATE`.
- `quansio-dev-seed --live` seeds the fixed `LIVE_IDS` rows for harnesses.
- The CLI's `targets list` now reads the served shape (`items`).

### Desktop (`apps/desktop/src/live/`, the task's path)

- `session.ts`: decode the projection; `agentInputAllowed` is the server's fence;
  `takeoverAllowed` / `handbackAllowed` offer only the legal control (no handback for a
  policy pause); `applyEvent` applies a control event only when it names this session id
  — a replacement session cannot slip in; the two command builders refuse anything but a
  `bsn_` session or a `tgt_` target.
- `terminal.ts`: `TerminalReplay` starts at the durable cursor, accepts
  `terminal.bytes {session_id, from, bytes}` in order, trims a re-sent overlap, reports a
  gap instead of inventing bytes, refuses another session's bytes; `resume` moves to the
  later of the local offset and the durable cursor and reports what was lost.
- `frames.ts`: `browser.frame` for this session only, base64 image data only, agent
  cursor and action, newest-sequence wins.
- Main: `live.targets|session|takeover|handback|attach|detach`; `live.attach` opens a
  cursor-less `target:<tgt>` subscription beside the workspace one and pushes
  `live.frame`; refusals are `Settled` data. Preload allowlist and API extended (pinned by
  `preload.test.ts`).
- `renderer/Live.tsx`: the `live-browser-terminal` surface — pause banner (agent driving /
  paused by you / paused by policy), same session id shown before and after, takeover and
  handback controls, navigation and tabs, screencast frame with the agent cursor, the
  native-computer row, and the terminal replay with its cursor and gaps.

## Acceptance → tests

- *No hidden replacement session is created on takeover.* `live_sessions.rs`
  `a_takeover_fences_the_same_session_and_parks_the_run` (session count stays 1, same id
  in the result and projection); `live.e2e.test.ts` (same id through takeover, restart,
  handback; the target lists exactly `[bsn_…]`); `live.test.ts` (a control event for
  another session leaves the view unchanged).
- *Agent input is fenced while user owns control.* Same server test: the row is
  `user/paused_takeover`, `agent_may_drive=false`, the run is `WAITING_TAKEOVER` and
  `next_safe_action` returns `AwaitHandback {session_id}`; after handback it is
  `Continue`. The native form: `a_native_computer_takeover_drives_the_control_row_with_its_fence`
  (`agent_may_input=false`, a call in flight keeps the takeover pending). The E2E asserts
  the projection's `agentInputFenced` before, during and after.
- *Paused by user control or policy.* `a_policy_pause_is_shown_and_no_handback_undoes_it`
  (reason `policy`, handback and takeover refused, no event written).
- Required tests: single-session test (`live.test.ts`), takeover E2E and reconnect test
  (`live.e2e.test.ts`: a new bridge over the same session file finds the same session
  still user-held and the replay resumes at the durable cursor 128).

## Status

APP-007 depends on EXEC-010, which is BLOCKED_EXTERNAL (implementation complete; the
Windows 11 UI Automation boundary needs a Windows host). The validator forbids PASS while
a dependency is not PASS, so APP-007 is recorded BLOCKED_EXTERNAL with
`implementation_complete: true`. Real CDP screencast frames and PTY bytes need the worker
control channel from an execution substrate (EXEC-003/004/005, BLOCKED_EXTERNAL on this
host); the frame shapes are DOMAIN.md §9.3's and are unit-tested.
