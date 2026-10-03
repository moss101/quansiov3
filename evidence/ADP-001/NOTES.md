# ADP-001 evidence notes

The managed browser is hardened in four places: the worker's driver (`crates/qworkerd/src/browser/`), the
control-plane row and decisions (`crates/machine/src/control/browser.rs` + `browser_guard.rs`), the Tool
declarations and their side-effect classes (`crates/tools`, `config/tools.yaml`, `config/effects.yaml`),
and the typed states in the live surface. Authority text is DOMAIN.md §8.4.1 (A–F), §7.5, §12 rule 6,
§14, §15. `RECONCILIATION.md` records what existed and what was and was not touched.

## What proves each acceptance statement

| Acceptance statement | Proof (all against a real headless Google Chrome unless noted) |
|---|---|
| A stale revision, snapshot or capture never mutates the page | `crates/qworkerd/tests/browser_fencing.rs`. The page's button asks a real HTTP server for `/hit`, so "not clicked" is a server-side count of zero, never an inference. Refused: an unknown revision, an unknown snapshot, a ref whose element changed name (fingerprint), an element covered by an overlay (hit test), a revision left behind by a navigation, a snapshot spent by its own click, a capture spent by its own click, and a point read before the page scrolled. A click on the *current* page lands exactly once, so the zero counts mean something. |
| An in-doubt browser action is not retried and is surfaced as in-doubt | `crates/qworkerd/tests/browser_ledger.rs`. The handler keeps the renderer busy so Chrome really never acknowledges the mouse event inside the action timeout; the server saw the click once. Result: `status: in_doubt`, `ledger_outcome: OUTCOME_UNKNOWN`, `retryable: false`, `requires_new_observation: true`. A redelivered call, a restart (the action's future is aborted after its `start` line is durable, then a second driver opens the same directory) and a third process all answer in doubt without touching the page: the server count stays 1. Same key, different arguments: `BROWSER_ACTION_REPLAY_CONFLICT`. A completed key replays its stored result. A reconciled key stays spent. |
| A secret never appears in model context, tool arguments at rest, ledger digests, logs or events, and is refused outside its allow-list | `crates/qworkerd/tests/browser_secrets.rs`: the canary is searched for in the model-facing reply (JSON and `Debug`), a following observation, the arguments, the driver's guard report and every file under the session directory; the journal digest equals the digest of the *placeholder* form and differs from the digest of the substituted value; origin refusals at the top frame, at the target frame (a real cross-origin iframe on a third server), for an allow-list entry that is not an origin, and for a page that navigates while the secret is being resolved. `crates/machine/tests/browser_guard.rs` (real PostgreSQL): the value is in no stored column but the sealed ciphertext; refusals name no value. |
| Plain text into password / one-time-code / card fields is refused | Same suite: refusal for each of the three field kinds, for literal characters mixed into a placeholder, and for a printable key (and a chord) while focus is in a sensitive field or in a frame whose focus the driver cannot read. Every refusal happens before a secret is materialized (resolver call count 0). |
| Agent input cannot dismiss a certificate error or grant a permission | `crates/qworkerd/tests/browser_guard.rs`: the tool surface has no tool and no argument for either (unknown tools and extra fields are `BROWSER_INVALID_REQUEST`), and afterwards Chrome still says `denied` / the certificate error is still pending and the page never loaded. `crates/machine/tests/browser_guard.rs` (real PostgreSQL): an agent actor, or a user id that is not `usr_`, is refused and the row is byte-for-byte unchanged; only camera and microphone can ever be granted. A scan asserts the only receipt constructors are the two store functions that call `require_user` and a `test-support`-gated test constructor. |

Dialogs, downloads, permissions, certificates, gesture, labelling and the strict tool surface are in
`browser_guard.rs` (15 tests); the contract between DOMAIN.md, the generated catalog and the code is
`browser_contract.rs`; the registry side is `crates/tools/tests/browser_side_effects.rs`.

## Mutation checks

`mutation-checks.log` (script: `mutation-checks.py`): each guard was broken on purpose — 29 mutants:
revision, fingerprint, hit test, invalidation, scroll, pixel warning, replay gate, digest conflict,
restart doubt, queue, tier ceiling, the sent flag, plain text in sensitive fields, secret origin (top and
frame), printable keys, digest over the substituted text, dialog fence, popup gesture, dialog id,
certificate pending, permission denial, navigation validation, schema validation in and out, the
side-effect mapping check, user-only decisions, report-undoes-decision, secret label match. A mutant is
KILLED only if it compiled (`COMPILES`), the unmutated suite passed first (`CONTROL-PASSED`) and a named
test then failed; a mutant that does not compile is `NO-VERDICT`, never KILLED. In the final run all 29
compile and all 29 are killed. Two earlier verdicts were not clean and were replaced rather than
counted: a mutant whose `if false` match guard left the match non-exhaustive did not compile
(NO-VERDICT; replaced by one that stops the sent flag being set), and a mutant that disabled only the
scheme check of `validate_navigation` SURVIVED because `origin_of` refuses every non-web URL on the next
line (an equivalent mutant — defence in depth; replaced by one that disables the whole validation).
The unit tests for the result-validation step (`tools.rs`) were added after the first run showed that
nothing in the Chrome suites produced a schema-invalid result.

## Traps met, and what was decided

- **Chrome says nothing about a `download` link until the first byte.** A server that sends an
  attachment's headers and no body gives the browser protocol no event at all for a `download` link, so
  there is nothing to time out. A *navigation* to an attachment does produce `requestWillBeSent` and
  `responseReceivedExtraInfo`, so the start timeout is implemented there (`AttachmentPromised`) and the test
  uses a plain link; the size cap and the stall timeout are on `downloadProgress` and work for both.
- **A certificate error freezes the page.** While Chrome waits for a decision, `Runtime.evaluate` on any
  connection hangs, so the driver never evaluates in a tab with a pending certificate error, and the
  tests read the driver's own state (and the page only after the error is cancelled or decided).
- **Chrome reveals the certificate only after a *continue*.** The decision is therefore recorded per
  origin and error, and bound to the leaf's SHA-256 as soon as the browser reveals it
  (`certificate_sha256`, checked against `openssl`'s DER digest in the suite). After a continue Chrome
  raises a second certificate error for the browser's own favicon request; an error that is not a
  navigation the tab started is cancelled by the driver (failing closed) instead of fencing the tab
  behind a decision nobody can see the point of.
- **A dispatch that blocks.** `Input.dispatchMouseEvent` does not return while an `alert()` is open, so
  an action runs its CDP calls in a spawned task and returns `dialog_pending` the moment the dialog opens.
- **`effects.yaml` section order.** `effects_taxonomy_and_model` builds malformed taxonomies by string
  replacement on the file. The `browser_side_effects` mapping therefore sits before `effect_classes` and
  uses the key `settles_as`, so the taxonomy rows stay the only lines that say `effect_class:`.
- **`ApiErrorCode::retryable` stays false for `BROWSER_*`.** `api.rs` asserts a retryable API error is a
  5xx/429; the browser flags are the Tool error's, in DOMAIN.md §15.
- **Test concurrency.** Fifteen headless Chromes launching at once starved each other of their debugging
  port (`Connection refused`, as in the EXEC-009 flake). `tests/common` limits one test binary to four
  browsers and waits for a real `/json/version` answer, not a bare TCP connect.
- **Worktree.** `gen_contracts.py --all`/`--check` cannot run the Rust generator from a worktree under
  `.claude/worktrees` (the outer workspace claims `contract-gen`); the catalog, OpenAPI and SDK steps were
  run individually. The proto files are untouched, so the Rust and Python bindings do not change. The
  `contract` tests that need the Rust generator fail only for that reason in a worktree.

## Deferred, and why

- `ResolveBrowserCertificate` and `DecideBrowserPermission` have no HTTP handler (the catalog reports "no
  handler in this build", as for `ImportBrowserSession`) and the desktop has no button for them. The
  control-plane half (user-only, recorded with the user's identity, returning the receipt) is real and
  tested against PostgreSQL; the worker half acts only on a `DecisionReceipt`. The live surface shows the
  pending decision.
- The control channel that carries `BrowserDriver::guard_report()` to `record_guard_report` and receipts
  back, and a production composition of `BrowserDriver`: qworkerd has no daemon yet (QA-002 `KNOWN_GAPS`,
  EXEC-002). The shapes are checked against each other: the driver's report parses with the machine's
  strict `GuardReport::from_json` in the ledger suite.
- Out-of-process iframes: reported in `frames_skipped`.
- The `DecisionReceipt` test constructor lives behind the machine crate's `test-support` feature, enabled
  only by qworkerd's dev-dependency. A cross-crate real-boundary test of "PostgreSQL mints the receipt,
  Chrome acts on it" cannot exist while qworkerd may hold no database client (arch rule), so each half is
  proved against its own real boundary.
