# APP-007 over EXEC-009 — watching the agent work, and richer input

**Why this exists.** The previous step let a person take a worker's browser over and see it, but only after taking it over:
watching the agent work — the thing people actually want from an agent with its own computer — was not offered. And once in
control a person could only click and type letters: no Enter, Backspace, arrows, Tab or scroll, so most pages were unusable.

| Item | Decision |
|---|---|
| Canonical owner | `crates/server/src/worker` (viewers, stream control), `crates/server/src/api/stream.rs` (registers viewers), `crates/server/src/api/live_sessions.rs` (the command), `crates/qworkerd` (stream, snapshot, key, scroll, tolerant reader), `crates/machine` (wire), `apps/desktop` (viewer) |
| Persistent state touched | none new |
| Contracts | wire `Screencast.refresh`, `UserKey`, `UserScroll`, `USER_KEYS`; `SendBrowserInput` kinds `key` and `scroll` (DOMAIN.md §8.4.2). The command name is unchanged, so no regenerated artifact moved |
| External effects | none new |
| Real boundaries | Chrome, PostgreSQL, a Unix socket, a real WebSocket against the served router. Nothing simulated in the Rust tests |
| Rollback | revert the branch; or start the worker without `--chrome` |

## Design choices worth reviewing
* **The stream follows the viewer, with no timer.** A live socket subscribed to a `target:` channel is counted as a viewer by a
  guard that is released however the socket ends. First viewer starts the stream, last viewer leaving stops it. The alternative
  (a lease the viewer renews) needs a timer on the server, which D-004 forbids.
* **Any member of the tenant can start a target's stream by subscribing to its channel** (the existing channel rule; this
  step did not add per-workspace authorization to `target:` channels). The cost is bandwidth, and the frames are as visible as the
  channel already was. Worth tightening; not done here.
* **A late joiner is given a snapshot, and numbers never go backwards.** Chrome only sends a frame when the page changes, so a
  viewer who arrived on a static page saw nothing. A one-off `Page.captureScreenshot` is sent on arrival. Frame numbers are one
  counter per session because the desktop drops any frame older than the latest it has seen; restarting the number would have made
  existing viewers ignore everything.
* **The worker skips a frame it cannot read instead of ending the connection.** Found while writing a hostile-input test: one bad
  frame, or any order a newer server learns, disconnected the worker. Skipping one means a dispatch that was skipped is never
  answered, which the server already settles as unknown and does not retry.
* **Keys are an allowlist of 13 named keys** and printable characters are text; chords are never forwarded. Validated at the
  control plane and again by the worker's driver, and the test sends hostile orders straight to the worker to prove the second layer.

## Mistakes found and fixed on the way (so they are not repeated)
* My first "the stream stops" proof was vacuous: it asserted silence on a page that never changes. It survived a mutant that
  never stopped the stream. The probe showed the real code delivers 0 pictures and the mutant 15, and an animated page with a
  positive control ("pictures really do flow while someone watches") now makes it a real test.
* I swept Finder's `.DS_Store` files into a commit with a broad `git add -A`; they failed the workspace check on a clean clone.
  Untracked and ignored.

## Known limits
* **No drag, right click or modifier chords**, and one HTTP command per key press (scroll is coalesced).
* **The desktop viewer was not exercised in a real window.** Mapping, builders, the handler and the fact that the view uses only
  that channel are unit-tested; the path from a command to the page is tested against a real Chrome.
* **The daemon serves one thing at a time**, so a takeover or an input waits for an in-flight browser action.
* **A decision relayed to an absent worker is not retried.**
* **Screenshot pixels** still do not cross the control channel to the model (only the live view carries pictures).
* **Frames are JPEG, base64 over JSON lines** — fine locally, wasteful across a network; a binary channel is the obvious next step.
* **APP-007 stays `BLOCKED_EXTERNAL`** (EXEC-010's Windows boundary).

## Not caused here
* `local-files-watcher.test.ts` (a real OS file watcher) failed once in a full parallel run and passes alone 3 of 3.
* `collaboration.e2e.test.ts` failed once in a full parallel run with `INTERNAL: illegal agent_thread transition: ACTIVE -> ACTIVE` and passes alone 3 of 3. An INTERNAL from two concurrent posts looks like a real race in the collaboration/agent-thread transition rather than test timing; a separate task was opened and nothing here touches that code.
