# ADP-001 reconciliation

Recorded before implementation (AGENTS.md task loop, RECONCILE). Coverage: `GENUINE_GAP` — the managed
browser (EXEC-009) exists and is correct for what it set out to do, but none of the hardening below
exists: nothing fences a mutating action to the page the model observed, nothing makes a browser action
idempotent or gives an unknown outcome its own result, nothing keeps a credential out of the model's
hands while typing, and a page can still raise a dialog, start a download, ask for a permission, present a
bad certificate or open a window without any typed state.

## Canonical owner

`Execution` (task `owner`), paths from the task: `crates/qworkerd/src/browser/`,
`crates/machine/src/control/browser.rs`, `crates/tools/`, `config/tools.yaml`, `config/effects.yaml`.
Everything new lives in those paths, with four small, necessary exceptions outside them, each forced by an
authority document rather than chosen:

- `crates/server/src/api/error.rs` — `ApiErrorCode` is a closed enum checked against the generated
  `schemas/catalog/errors.yaml`; DOMAIN.md §15 gains the `BROWSER_*` family, so the enum gains it too.
- `crates/server/src/api/live_sessions.rs` — the existing live projection carries the new `guard` object
  and each tab's `document_revision`; one match arm for the new store errors.
- `crates/server/src/runtime/turn_loop/dispatch.rs` — `host_owner` named five browser tools; it now names
  the `browser.` family, so the new tools are not attributed to the file-tool owner.
- `apps/desktop/src/renderer/Live.tsx` and `apps/desktop/src/live/session.ts` — the task's own scope
  guard: surface the new typed states in the existing live surface.

## Existing code and its coverage

| Code | Classification | What it did | What changes |
|---|---|---|---|
| `crates/qworkerd/src/browser/cdp.rs` | covered, extended | CDP client over one socket; one-consumer event queue | adds a broadcast subscription (the queue discards what `wait_for_event` is not waiting for, and a full queue stalled the reader), a per-call timeout and `close_page` |
| `crates/qworkerd/src/browser/mod.rs` | covered, kept | selector-based observe/click/type/web_fetch, page-semantics classification, takeover rules | untouched in behaviour; it is the low-level layer the driver sits on. `extract`, `bound` and a base64 helper become crate-visible |
| `crates/machine/src/control/browser.rs` | covered, extended | the `browser_sessions` row, takeover/handback | `Tab.document_revision`, `BrowserSession.guard` |
| `crates/tools` | covered, extended | registry, schema vocabulary, `plan_call` | `side_effect` on a declaration, `SideEffectMapping`, a check that a browser tool's class agrees with it |
| `config/tools.yaml` browser block | covered, replaced | selector-based `browser.click` / `browser.type` at version 1 | the unfenced selector forms are not callable any more: click/type/screenshot/navigate become version 2, and `observe`, `press_key`, `handle_dialog` are new |
| `crates/server/src/effects/ledger.rs` | covered, not touched | keys an effect by `(class, resource, params_digest)`, refuses a re-reserve while in flight, never retries `OUTCOME_UNKNOWN` | nothing — see "Effect Ledger" below |

## Persistent state touched

One column on one existing table: `browser_sessions.guard_state JSONB NOT NULL DEFAULT '{}'` (migration
`0025`, additive, CHECKed to be an object). The tabs' `document_revision` rides in the existing `tabs`
JSONB. No new table, store, queue or effect path (AGENTS.md invariant 12; D-019 approves the task).

The worker keeps `actions.jsonl` in its session directory. It is not a ledger: it holds no policy,
approval or outcome the server has not been told, it is a replay guard that must survive a worker
restart so a started action is *in doubt* afterwards rather than forgotten, and the server holds the
doubt in `guard.in_doubt`.

## Effect Ledger

Not duplicated and not edited. The ledger already refuses a re-reserve of an in-flight key and never
retries `OUTCOME_UNKNOWN` (RUN-007). What it cannot know is whether input reached a page. The worker
reports `status: in_doubt` (`ledger_outcome: OUTCOME_UNKNOWN`) and the runtime's existing
`ToolDispatchOutcome::OutcomeUnknown` path settles it. qworkerd has no daemon yet (QA-002's
`KNOWN_GAPS`: `WorkerHost`, owned by EXEC-002) and the server composes no browser Tool host, so
`BrowserDriver` is — like EXEC-009's driver before it — a tested library without a production
composition. The mapping `ReplyStatus::InDoubt → OUTCOME_UNKNOWN` is therefore stated by
`ToolReply::ledger_outcome` and asserted in the suites, but not exercised through the server end to end.

## Contracts and events changed

DOMAIN.md: §0 glossary (DocumentRevision, Snapshot/Capture, In-doubt outcome, SecretPlaceholder,
DecisionReceipt), §7.5 (`side_effect`, three new tools, the side-effect table), §8.4 + new §8.4.1 A–F,
§12 rule 6, §14 (`ResolveBrowserCertificate`, `DecideBrowserPermission`), §15 (the `Browser` family and a
flags table: `retryable`, `requires_new_observation` for every code). Generated catalogs, OpenAPI and the SDK
operation tables are regenerated, not hand-edited. No event type changes.

## External effects and their classes

Unchanged taxonomy. The mapping adds no class: read → `read.external`, read_sensitive → `read.internal`,
local_mutation and potential_external_effect → `computer.input.privileged` (what click/type already
declared; the driver refines a click to `message.send`, `record.delete`, `payment.execute`,
`credential.access` from the control it lands on), open_world_mutation → `network.egress.new_destination`.

## Capabilities and approvals

Capability only narrows: the driver adds refusals. A secret placeholder needs `credential.access`
(tier 4); a call whose approved tier is lower is refused before any secret is resolved. A click whose
classified consequence exceeds the approved tier is refused before dispatch.

## Migrations

`0025_browser_guard_state.sql`, forward-safe: `ADD COLUMN ... DEFAULT '{}'` plus one CHECK. Exercised by
every `quansio-machine` suite, which migrates a fresh database.

## Real boundaries and the environment they need

- A real headless Google Chrome (the same host install EXEC-009 used). No credentials. Suites report
  `BLOCKED_EXTERNAL` without it.
- Real HTTP servers on loopback (in-test, tokio). A real HTTPS server with a self-signed certificate for
  the certificate suite: `openssl` makes the certificate and `python3` serves it; the suite reports
  `BLOCKED_EXTERNAL` when either is missing.
- Real PostgreSQL for the machine suites (`QUANSIO_TEST_POSTGRES_URL`), on a private database.

No secret, token or API key is read from the environment by any test; the secret values are canaries
created in the tests.

## Rollback and recovery

Rollback is forward-fix (migrations policy). The column defaults to `{}`; a surface that does not read
`guard` is unaffected. Tool versions: callers planned against `browser.click` v1 cannot dispatch (no host is
wired in production). Recovery: a restarted worker opens its journal and every started-but-unfinished
action is `in_doubt` from the first call, written back so a second restart agrees.

## What is deliberately not in this task

- The HTTP commands behind `ResolveBrowserCertificate` and `DecideBrowserPermission`, and the desktop
  controls that would send them: the catalog and the control-plane store (user-only, tested against
  PostgreSQL) exist; the server handlers and buttons do not. The live surface shows the pending decision.
- Wiring the browser Tool host into the server's composition, and the control channel that carries the
  worker's guard report to `record_guard_report` and a decision receipt back. Both halves are tested
  against their real boundaries separately.
- Cross-process (out-of-process) iframes: a frame whose accessibility tree the page endpoint will not
  serve is reported in `frames_skipped`, never guessed at.
