# APP-007 reconciliation (2026-09-21, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `apps/desktop/src/live/session.ts` was a 36-line
stub whose "takeover" flipped two fields of an in-memory object, no server surface
existed for a browser session, and the "E2E" never reached a server
(docs/review/2026-09-18-m5-placeholder-audit.md). The 2026-09-19 correction reset it to
NOT_STARTED. The branch of that era is parked as `stale/APP-007-live-surface-2026-09-15`.

- **Canonical owners.** `apps/desktop/src/live/` (the task's path): the live
  browser/computer/terminal surface. Behind it, only authorities that already exist:
  `crates/machine/src/control/browser.rs` (BrowserSession row, EXEC-009: takeover fences
  agent input on the same row, never a second session), `crates/machine/src/control/
  terminal.rs` (TerminalSession row with the durable cursor, EXEC-006),
  `crates/machine/src/computer_use/store.rs` (ComputerControl row, EXEC-010), the
  runtime's ProtocolState `browser_control` and `WAITING_TAKEOVER` (RUN-001/RUN-003:
  `next_safe_action` returns `AwaitHandback` while the user holds the session), and the
  client stream (CORE-009/APP-001: `target:tgt_…` channel, transient `browser.frame` and
  `terminal.bytes` frames).
- **Existing code, coverage PARTIAL, kept:** all of the above; the desktop stream client
  already parses live frames and the shell already forwards them to the renderer
  (APP-003 build item 3). `RequestTakeover`, `Handback` and the `/v1/targets`,
  `/v1/browser-sessions/{id}` projections are catalogued (DOMAIN.md §14) but had no
  handler and no route.
- **Gaps.** No server handler for `RequestTakeover`/`Handback`; no route for the two
  projections; the runtime never parks a run for a takeover nor releases it on handback;
  the desktop has no session state, no takeover controls, no pause reason, no terminal
  replay, no target-scoped live subscription, no page; no test touches a server.
- **Ownership decisions (recorded, not assumed).**
  - *The session row is the fence, the run is parked by it.* `RequestTakeover
    {session_id}` moves the BrowserSession row to `control_holder=user,
    status=paused_takeover` through the machine authority (same row, same id), writes
    `browser_control {holder: user}` into the run's ProtocolState and transitions a
    RUNNING run to `WAITING_TAKEOVER` — one transaction, one `browser.control_changed`
    event, one `run.waiting` event, one AuditEntry. `Handback {session_id}` is the only way
    back: the row returns to the agent, ProtocolState's `browser_control` is cleared and
    the run resumes (`run.resumed`). A handback of a session the user does not hold is
    refused (`CONFLICT_STATE`), so it cannot resume a session policy paused
    (`paused_policy`); a takeover of a session that is not active is refused the same
    way. `RequestTakeover {target_id}` / `Handback {target_id}` are the native-computer
    form over the ComputerControl row (EXEC-010's request → complete → handback, with
    its generation fence); both forms audit and both name the run they park.
  - *No hidden replacement session.* The command result and the projection carry the
    session id the row had before; the desktop's state reducer refuses a control event
    that names another session id, and the E2E asserts the id is the same before and
    after takeover, handback and reconnect, and that the tenant still has exactly one
    browser session.
  - *Agent input is fenced by the server, shown by the client.* The projection computes
    `automation {paused, reason: user_control|policy|null, agent_input_fenced}` from the
    row and ProtocolState; the desktop disables nothing on its own authority — it renders
    what the projection says and the run's `WAITING_TAKEOVER` status.
  - *Terminal replay resumes from the durable cursor.* The projection lists the target's
    TerminalSessions with `cursor` (EXEC-006's durable byte offset); the desktop's replay
    buffer accepts `terminal.bytes {session_id, from, bytes}` frames only in order,
    trims the overlap of a re-sent chunk, reports a gap instead of guessing, and on
    reconnect resumes from `max(local offset, durable cursor)` — the same session id, the
    same offset, nothing re-rendered.
  - *Live frames are the server's, subscribed per target.* The desktop keeps its one
    durable workspace subscription (APP-003) and opens a second, cursor-less stream on
    `target:<tgt>` for the live view (`live.attach`/`live.detach`); frames reach the
    renderer only through the preload bridge (`live.frame`).
- **What this cannot prove here.** The real screencast (CDP frames from Chrome inside an
  execution target relayed by qworkerd) and PTY bytes need the worker control channel to
  the server, which no execution substrate on this host provides (EXEC-003/004/005 and
  EXEC-010 are BLOCKED_EXTERNAL). The E2E proves the real server, the real rows and the
  real stream; the frame shapes are DOMAIN.md §9.3's and are unit-tested.
- **End state.** APP-007 depends on EXEC-010, which is BLOCKED_EXTERNAL
  (implementation complete; the Windows UIA boundary needs a Windows 11 host). The
  validator forbids PASS while a dependency is not PASS, so this task ends
  BLOCKED_EXTERNAL with `implementation_complete: true` and a blocker naming EXEC-010.
