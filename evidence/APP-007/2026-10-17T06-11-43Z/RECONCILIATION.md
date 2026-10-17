# APP-007 over EXEC-009 — relaying a worker's browser to the control plane and the live view

**Why this exists.** After browser routing (EXEC-009), `browser.*` calls ran on a worker's own Chrome, but the control plane
could not see that browser or take it over: the driver's guard report (dialogs, certificate errors, calls in doubt) stayed on
the worker, takeover and handback changed a database row the worker never heard about, a person's certificate and
permission decisions had no way to reach the page, and the live view had no pictures and no way to click.

| Item | Decision |
|---|---|
| Canonical owner | `crates/qworkerd` (daemon loop, runtime, screencast pump), `crates/server/src/worker` (hub), `crates/server/src/api/live_sessions.rs` (relay and the new command), `crates/machine` (wire frames, receipt wire form), `apps/desktop` (viewer, command builder, bridge) |
| Existing code | the driver's `request_takeover`/`handback`/`user_click`/`user_text`/`resolve_certificate`/`apply_permission_decision`/`guard_report`, `BrowserSessionStore::record_guard_report`, `LiveHub`, the desktop's takeover and decision UI: reused unchanged |
| Persistent state touched | `browser_sessions.guard_state`, written only through `record_guard_report`. No new table |
| Contracts | wire frames (DOMAIN.md §8.4.2), the command `SendBrowserInput` (DOMAIN.md §14; catalog, OpenAPI and SDKs regenerated), `ReceiptWire` |
| External effects | none new. A person's input is delivered to the page; it settles no Effect (it is the person acting, not the agent) |
| Real boundaries | Google Chrome, PostgreSQL, a Unix socket, the HTTP command surface. Nothing simulated in the Rust tests |
| Rollback | revert the branch; or run the worker without `--chrome` |

## Things to review (not buried)
* **I extended a security invariant test.** `crates/machine/tests/browser_guard.rs::a_receipt_has_no_constructor_outside_the_control_plane_and_the_test_feature`
  pinned "a receipt is built in exactly two places, both requiring a user". A worker has to rebuild one from the wire, so
  there is now a third: `DecisionReceipt::from_control_channel`. The test now also pins that there is exactly one such
  function, that it takes only the wire form, and that the **only** caller in the workspace is `qworkerd/src/daemon.rs`
  (a mutant that adds a mention elsewhere is killed). The type-level "only the control plane can mint one" is weakened to
  "only the control-channel reader can rebuild one"; the trust it carries is the channel's, which only the lease holder can write.
* **I broke two desktop E2E suites on `main` with my first worker merge and did not notice.** That merge made a served server
  refuse `fs.*`/`terminal.exec`/`process.spawn` without a worker. The conversation and trust E2E suites run the stub model,
  which asks for an absolute path no real worker would read, and expect the stub host's fake success. I only ran the Rust
  crates then; the desktop E2E needs the dev stack, which was down. They are fixed here by an explicit opt-in,
  `QUANSIO_CONFORMANCE_STUB_HOST=1`, that logs a warning that nothing is executed; the served default stays fail-closed.
  That flag is a way to run the stub in a served binary, and nothing technical stops someone setting it in a deployment.
* **Individual inputs are not audited**, and any workspace member who may author work can send input while the session is
  user-held (the session does not record who took it over). Both are stated in DOMAIN.md §8.4.2.

## Known limits
* **Input is crude.** A click and printable characters (as inserted text). No Enter, Backspace, arrows, scroll, drag, right
  click or shortcuts; each keystroke is one HTTP command. Enough to prove the path and to fill a simple field; not a remote desktop.
* **Frames flow only while a person holds the session.** Watching the agent work is not offered yet (the `Screencast` order
  exists; no command asks for it).
* **The daemon serves one thing at a time**, so a takeover waits for an in-flight browser action (up to its timeout). Concurrent
  dispatch is still open.
* **A guard report is sent at action boundaries and after each order**, so a certificate error that appears between actions
  is reported with the next one.
* **A decision relayed to an absent worker is not retried.** It is recorded either way and the response says
  `worker_relayed: false`; the worker's own bounded wait cancels an unresolved certificate error.
* **Screenshot pixels** still do not cross the control channel (only `evidence_bytes`).
* **The desktop viewer was not exercised in a real window.** Its mapping, builder, handler and the fact that the view calls
  only that channel are unit-tested; no Electron/Playwright run clicked a picture. The path from a click to the page is proved
  below the UI (HTTP command → hub → worker → Chrome) against a real Chrome.
* **APP-007 stays `BLOCKED_EXTERNAL`** (EXEC-010's Windows boundary); nothing here changes that.

## Pre-existing failures found, not caused here
* `tests/automation.e2e.test.ts` creates a 1-second interval routine; ADP-007 (`dfc4a13e`) added a 60-second plan minimum and
  the test was not updated. Fails on `main`.
* `tests/work.e2e.test.ts` passes alone 3/3 and failed once in the full parallel run: load-sensitive.
* Three server test binaries overflow the stack in debug builds on `main` (a separate follow-up is in flight); they run here
  with `RUST_MIN_STACK=16777216`.


> **Superseded after review:** the stub opt-in described above was dropped when `main` landed a better fix for the same breakage (the conversation and trust E2E suites now run on a real attached worker, EXEC-002). The served default is unchanged: worker tools are run by an attached worker or refused `WORKER_UNAVAILABLE`.
