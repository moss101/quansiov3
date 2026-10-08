# ADP-009 notes

Status at hand-off: **BLOCKED_EXTERNAL with `implementation_complete: true`** (D-017: a dependency, ADP-002, is itself BLOCKED_EXTERNAL, and
`real_boundary` is true, so this task can never be PASS from here).

## Built (all on the existing authority path, no new service, store, transport or Go)

1. **MCP client** (`crates/tools/src/mcp.rs`, `crates/qworkerd/src/mcp/`): external tools enter the Tool Registry as `mcp.<server>.<tool>`,
   untrusted-external, classified by `config/mcp.yaml` (unclassified = tier 3/4, never a read), narrowed by the Capability Projection
   (`connector:mcp.<server>/<tool>`), journalled and settled in the Effect Ledger. Real MCP server processes (stdio and loopback HTTP) in tests.
2. **Composition sandbox** (`crates/qworkerd/src/compose/`, `compose.run`): bounded QuickJS (memory, stack, wall clock, call count), no
   ambient I/O, every `call` a governed nested tool call with its own receipt.
3. **Payment-field flow**: `SupplyPaymentInput` (a person's command, desktop channel only), one-time values in the secret broker with a TTL,
   `browser.fill_payment_field` (match, take, origin, mask, insert, verify, drop), `payment.execute` stays tier 4 always asked.
4. **Password manager (human-only) and explicit browser-data import** (`apps/desktop/src/main/secrets/`, `ImportBrowserSession`,
   `browser.session.import` tier 4 always asked).
5. **Remote control** (DOMAIN.md 8.6.2): host-side authorization and protocol over the existing live-frames WebSocket (no LiveKit/WebRTC, OD-008).
   Viewer identity from the server session; per-session owner consent; view vs control grants; typed bounded input protocol; separate bounded
   clipboard channel (text or <= 2 files, <= 64 KiB, plain names); input only when authorized AND capture ready AND unlocked AND Accessibility and
   Screen Recording granted AND the computer's report is fresh; ComputerControl fence; Handback ends sessions; one session per computer; idle,
   expiry, sign-out and Stop end it with the reason in the event and audit chain; persistent "being watched/controlled" indicator entry with an
   immediate Stop (activity tracker, not dismissible). The computer's own side (`crates/qworkerd/src/remote.rs`) re-checks everything.

## Deferred (recorded, not hidden)

* **Passkeys (item 6) are deferred, design constraints recorded.** The cross-device flow needs the OS WebAuthn stack on a real device and a
  browser top frame; nothing here proves it. When built: **human-only** (a person's gesture, never an agent tool and never in the Tool
  Registry), **top-frame only**, **HTTPS origins only** (relying-party id = the page's registrable origin, no override), **never exposed to
  model context** (no credential id, challenge or assertion in any tool result or event payload), and **cancelled on navigation** (a pending
  ceremony dies when the top frame navigates or the tab changes).
* **Production capture, injection and conditions ports for remote control** over the native helper are not built. The rules are proven with
  stand-in ports (`crates/qworkerd/tests/remote_host.rs`) and a real server/WebSocket/PostgreSQL; the real screen needs ADP-002 permissions.
* **No viewer client** (web or desktop) exists: the viewer side is the protocol. The desktop only shows the owner's indicator/answers.
* **Normalized coordinates**: pointer coordinates are pixels of the last frame shown (bounded by that frame), which carries the same guarantee.
* Remote sessions are in memory: a server restart ends them. Not exercised: two server processes sharing a session (single process by design).

## BLOCKED_EXTERNAL
* ADP-002 (dependency, BLOCKED_EXTERNAL / EXEC-010 native computer use): `QUANSIO_TEST_MACOS_COMPUTER_USE=1` with Screen Recording and
  Accessibility granted, for remote control on a real screen and real native computer receipts under the sandbox.
* Passkeys: `QUANSIO_TEST_WEBAUTHN_DEVICE=1` (a real authenticator / cross-device flow) once built.
* Payment fill against a real card form with a provider test mode: `QUANSIO_TEST_PAYMENT_PROVIDER_*` (not run; Chrome-local fixtures only).
* Remote MCP servers needing credentials: `QUANSIO_TEST_MCP_REMOTE_URL` / `QUANSIO_TEST_MCP_REMOTE_TOKEN` (not run; local stdio/HTTP servers only).

## Mutation checks (`2026-10-08T12-00-00Z/mutation-checks.log`)
26 mutants over the new guards (remote control state machine, protocol, relay, host, desktop indicator; payment origin, import tier, sandbox
deadline, MCP registry, vault). Every kept verdict is preceded by CONTROL-PASSED and COMPILES. 25 KILLED (the sandbox one by timeout: a
sandbox whose deadline is ignored never returns). 1 SURVIVED and is equivalent: `payment-expired-value-taken` removes the `expires_at > $5`
filter of `take`'s SELECT, but `take` calls `sweep` with the same instant in the same transaction first, which overwrites every lapsed
value, so the filter is defense in depth and cannot be observed. The first pass logged PATTERN_NOT_UNIQUE for four patterns that `cargo fmt`
had reflowed; they were corrected and rerun (second pass in the same log), all four KILLED.

## Traps worth recording
* A mutation script's text patterns break when `cargo fmt` reflows the target: PATTERN_NOT_UNIQUE (0) is the symptom; format first.
* A `Window` rate limiter that is per second plus a test host whose readiness report is only 2 s fresh: a test must re-send `ready()` before
  every message that is expected to be admitted (the freshness window is the point, not a flake).
* Idle must apply to a view grant too, or a viewer that left keeps "being watched" lit for up to an hour; viewers `ping`.
* A committed `sdk/typescript/node_modules` symlink had slipped into the branch (commit b0e5c7d7); it is removed from the index by the closing
  commit. Never `git add -A` in this worktree (untracked node_modules symlinks).
* `conversation_surface::served_a_message_is_accepted_before_its_turn_and_the_reply_follows` has a 800 ms timing bound and failed once under
  full-workspace load (925 ms); it passes alone and was recorded as a load flake, not changed.
