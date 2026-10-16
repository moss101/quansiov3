# EXEC-009 browser routing — reconciliation

**Why this exists.** After the worker composition (EXEC-002), `fs.*`, `terminal.exec` and `process.spawn` ran on a real
worker, but every `browser.*` call still reached the conformance stub, which answers `{"ok":true,"stub":"conformance"}`.
The managed browser (EXEC-009, ADP-001) was built, hardened and tested against a real Chrome, but nothing the agent
did could reach it, and nothing in the served runtime ever opened a `BrowserSession`.

| Item | Decision |
|---|---|
| Canonical owner | `crates/qworkerd` (Chrome launcher, browser runtime, executor), `crates/server/src/worker` (session binding, fence, settle), `crates/machine/src/gateway` (`approved_tier` on the envelope) |
| Existing code | `BrowserDriver::dispatch` (ADP-001), `BrowserSessionStore` (EXEC-009), `EffectTaxonomy`, the egress broker's address classes: reused unchanged |
| Persistent state touched | `browser_sessions` rows, written only through `BrowserSessionStore` |
| Contract changed | `ActionEnvelope.approved_tier` (optional, not in `args_digest`) and DOMAIN.md §8.4.2, same commit |
| External effects | the browser tools' existing classes; no new class, no new tool |
| Real boundaries | Google Chrome, a local HTTP site whose counters show what the page really requested, PostgreSQL, a Unix socket. Nothing simulated |
| Config | worker flags `--chrome`, `--state-dir`, `--allow-private-network`. No new server variable |
| Rollback | start the worker without `--chrome` (browser tools fail `BROWSER_UNAVAILABLE`); revert the branch |

## Decisions worth reviewing
* **The server, not the model, chooses the session.** The model has no way to learn a session id and must not be able to
  name one, so the host replaces `session_id` with the one belonging to the call's own run. Run B naming run A's session and
  tab gets `BROWSER_TARGET_NOT_FOUND`. This changes an argument after the runtime has digested it; it is deterministic per
  run, and the driver's journal keys on `(effect_id, dispatch_token)`, so replay is unaffected.
* **The approved tier is derived, not supplied.** From the tool's declared effect class through the taxonomy.
* **Fail closed everywhere a stand-in could answer.** `browser.import_session` / `browser.fill_payment_field` and any unknown
  `browser.*` tool are refused, not stubbed; a reply the driver could not have produced is *unknown*, never success.
* **Chrome is started without `--remote-allow-origins`** (the test harness sets `*`; production must not), and its profile
  and journal are kept outside the workspace the file tools can read.

## Known limits (not hidden)
* **Navigation vetting is a pre-check.** It refuses loopback/private/link-local literals and names that resolve there, but
  not a redirect or sub-resource the page loads afterwards, and a name can resolve differently at fetch time (DNS rebinding).
  The target's network policy (EXEC-008) is the real control on a sandboxed substrate; a worker with no sandbox is as exposed
  as the person who started it. The "no `--remote-allow-origins`" choice has no test of its own.
* **Takeover is fenced on the server only.** The worker's driver is not told; if input reached it by another route its own
  fence would not know. Relaying `request_takeover` / `handback` to the driver is not done.
* **Screenshot pixels do not cross the channel**; the reply reports `evidence_bytes`. `browser.observe` (DOM + accessibility
  tree) is the primary path and works.
* **Guard report and live screencast are not relayed** to the server (tabs, dialogs, in-doubt list, certificate and permission
  decisions, the live view). Certificate and permission decisions therefore cannot be made from the app against a worker's
  browser yet.
* **A browser lives until the worker stops**; there is no idle or run-end close.
* **No end-to-end run with a real model.** The turn loop → model → browser path needs `QUANSIO_INTELLIGENCE_ENDPOINT` and a
  provider key: BLOCKED_EXTERNAL. What is proved is from the host down.
* **Mutant coverage:** M1–M7 killed. M6 first *hung* the suite instead of failing it; the test now bounds its waits.
