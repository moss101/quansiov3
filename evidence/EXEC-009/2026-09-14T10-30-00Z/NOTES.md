# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## acceptance

**Research and action workflows use the same browser stack.** `web.fetch` navigates the page through the
same `CdpConnection` and runs the same in-page extraction that an action does, so a fetch and a click
cannot disagree about what the page said. There is deliberately no separate scraper: the test drives
both through one browser and asserts a fetch's text and a click's observation come from the same
extraction.

**Browser action that causes external consequence routes through the Effect Ledger.** The browser's
contribution is the classification, and it is taken from the page's own semantics *before* the action
runs — the control's ARIA role, its accessible name, its href and the form it belongs to. A link to
another origin is `network.egress.new_destination` (tier 2), a control named "Buy now" is
`payment.execute` (tier 4), "Delete account" is `record.delete` (tier 3), a form submit with no stronger
signal is `record.create` (tier 2), and an in-page anchor or an inert toggle is *no effect at all* —
recorded as `None`, because a click that changes only the page has nothing for the ledger to settle. The
reservation and settlement themselves are RUN-011 and RUN-007, both `PASS`; what is verified here is that
a consequential action cannot reach them as an unclassified `browser.click`.

**`web.fetch` output is bounded, labelled `UNTRUSTED_EXTERNAL` and carries the source URL, retrieval time
and content digest.** All of it is asserted in one test against a real page: the text is cut to the bound
and `truncated` says so while `total_text_bytes` reports what the page had; the links are cut and counted
the same way; `requested_url` and the post-redirect `url` are both carried; `retrieved_at` is the instant
the caller supplied; `content_digest` is SHA-256 of the text that was returned, so a truncated fetch and a
whole fetch of one page digest differently and two whole fetches digest the same; and `trust` is
`UNTRUSTED_EXTERNAL` so nothing downstream can mistake page content for an instruction.

## the boundary is real, and it is Chrome

The task's `real_boundary` is `true`, and this is that boundary: every browser test launches the Google
Chrome installed on this host with the DevTools protocol on a loopback port and drives it. Nothing stands
in for Chrome and nothing asserts against a recorded fixture of what Chrome was expected to say. Chrome
152 was what ran. When no Chrome is present the suite prints `BLOCKED_EXTERNAL` and returns, so a host
without a browser is never a pass — that path is not what these results came from, because Chrome is here.

## the four bugs the tests caught

Writing this against a real browser found four defects that a fixture would have hidden:

- **`http_get` read to EOF**, and Chrome does not always close the connection, so the endpoint read hung.
  It now reads to `Content-Length` with a deadline.
- **`/json/new` needs PUT** on current Chrome; GET answered 405. PUT is tried first with GET as a fallback
  for older builds.
- **the accessibility tree is never empty.** Chrome always emits structural nodes for the document and
  body, so "no text and no accessibility nodes" never fired and a canvas page was reported as understood
  from the DOM. What says a page has content is a node that *names* something, so the count that decides
  is of named, non-structural nodes.
- **the extraction's form lookup compared a form element against the array of mapped plain objects**, so
  it was always `-1` and no control ever resolved its form — every form submit was classified as though it
  belonged to no form. The elements and the mapped shapes are now kept apart.

Two further mistakes were mine in the tests rather than the product, and are recorded here because the
distinction matters: a case that meant to exercise a button used an anchor with a nonsense href, and the
link count expected three anchors after one of them became a button.

## recorded_decisions

- **The session row is owned by machine control, not by the worker.** `crates/qworkerd` may not hold a
  Postgres client at all — the architecture gate forbids it, because a worker reaches the server only over
  its fenced channel — so `browser_sessions` is written in `crates/machine/src/control/`, the
  execution-model owner, and the worker carries the session state it is given and reports it back. Same
  split as EXEC-006's terminal session.
- **§8.4's rules are enforced at the durable boundary, not only in the worker.** A takeover is refused for
  a session that is not active, a handback is refused unless a person holds the session (so it cannot undo
  a policy pause), and the tab list is upserted in the same transaction as `current_url` so the two cannot
  drift. A second caller cannot disagree with the first about who holds the browser.
- **A takeover is a pause, not a new session.** The test asserts the tabs, the profile and the page all
  survive one, which is what §8.4's "no second session is created" means in practice.
- **`bsn_` was enforced by the schema and absent from the catalog.** `browser_sessions.id CHECK (id LIKE
  'bsn\_%')` has been there since 0001 and DOMAIN 8.4 defines `BrowserSession`, but 1.1's id table never
  listed it. Repaired additively within v1, exactly as `tsn_` was for EXEC-006.

## what is not claimed

- Native `Input.dispatchMouseEvent`/`dispatchKeyEvent` for takeover relay is not used: the driver clicks
  and types through the page's own DOM (`element.click()`, value plus `input`/`change` events), which is
  what a DOM-first driver should do. Real user input relay from a client during a takeover is APP-007's
  live-view surface and is not this task.
- Screencast frames are started and stopped and the stream reference is recorded; carrying the frames
  themselves to a client is the live-view transport, which is not built here.
