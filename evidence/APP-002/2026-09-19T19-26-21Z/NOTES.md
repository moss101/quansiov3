# APP-002 — identity, onboarding, workspaces and settings

## What was built (task branch `task/APP-002-identity-sessions`, merged b25da73 → 4f992d3 → fe88d7e)

The 2026-09-15 record was partial: an `IdentityStore` nothing called, OAuth and the magic
link as names in `auth_methods`, no session, no keychain, a 35-line onboarding plan
(docs/review/2026-09-18-m5-placeholder-audit.md). This is the account/session model of
DOMAIN.md §2.

**Server** (`crates/server/src/control/identity/`, migration 0013, `config/auth.yaml`):
- `auth.rs` — ES256 access tokens (`ring`, P-256 from `QUANSIO_AUTH_SIGNING_KEY_PKCS8_B64`
  or ephemeral with a logged warning), ≤ 15 min; opaque rotating refresh tokens stored
  hashed in rotation families — presenting a retired token revokes the family; magic-link
  challenges stored hashed, consumed once; find-or-onboard by normalized email. The
  pre-authentication tables are under forced RLS and reachable only through the
  transaction-local `quansio.auth_flow` gate the authority opens for its own statements.
- `oauth.rs` — Google/Microsoft/GitHub authorization-code flow with PKCE S256; endpoints
  and client ids from `config/auth.yaml`, client secrets from the environment variable the
  config names (the model gateway's custody rule); unconfigured providers fail closed
  (`PROVIDER_UNAVAILABLE`); the exchange is a port.
- API — `/v1/auth/magic-link/{start,complete}`, `/v1/auth/oauth/{provider}/{start,complete}`,
  `/v1/auth/refresh`, `/v1/auth/sign-out`, `GET /v1/me`. `Authorization: Bearer` resolves
  the scope on every route; the deployable believes nothing else unless
  `QUANSIO_TRUST_SCOPE_HEADERS=1` (logged; harness only). Magic links are delivered through
  a port: the console sink in development, the email channel when OD-009 lands.
- §14 `CreateWorkspace`, `UpdateWorkspace`, `UpdateUserSettings` — provider credentials
  must be `sec_` handles (raw secrets refused before anything is stored), execution target
  and primary model validated, effective settings and restart/reindex effects returned.
- Every workspace is provisioned with a root objective: without one a fresh workspace
  could not take a message (`PostMessage` anchors to the workspace's work node), which the
  new-user E2E found. Onboarding sets the user context so the `users` policy admits the
  insert as the application role — the earlier implementation had only ever run as the
  superuser.
- Contracts: DOMAIN.md §1 gains `ses_` (core `Prefix::Session`, proto `ENTITY_PREFIX_SESSION`)
  and §2 the session endpoints; the catalog and OpenAPI generators emit them; regeneration
  is drift-free and compatible.

**Desktop** (`apps/desktop`):
- `TokenStore` keeps only the refresh token, only encrypted through Electron `safeStorage`
  (the OS credential store); with no platform cipher nothing is kept at rest.
- `SessionManager` — sign-in, in-memory access token, one refresh on `AUTH_EXPIRED` then
  the same request, custody cleared on refusal, resume from the keychain at launch.
- Bridge: session mode beside the trusted-header harness mode; `auth.*`, `session.*` and
  `settings.*` channels on the allowlist with payloads validated as untrusted data; the
  OAuth URL opens in the system browser and returns via `quansio://auth/callback`; the
  renderer sees a projection, never a token.
- Onboarding: a pure state machine and screen — magic link or OAuth → execution target →
  security defaults → ready.

## Acceptance

1. **Fresh user completes onboarding without manual configuration files** —
   `new-user-e2e-real-server.log`: the real binary in its session-only default on an
   unseeded scratch database; a header-scoped request is refused `AUTH_REQUIRED`; magic
   link started through the bridge and read from the server console as a developer would;
   sign-in provisions the personal tenant, default workspace and root objective; `/v1/me`;
   execution target chosen with the restart effect reported; security defaults confirmed;
   a raw provider secret refused; a message posted with the bearer and its durable event
   received on the bearer stream; a restart resuming from the keychain alone; sign-out
   clearing custody so a third start is signed out.
2. **Settings merge is deterministic and most-restrictive for security controls** — the
   pre-existing merge test plus `identity_commands_manage_workspaces_and_settings_with_secrets_as_handles`
   (the user layer cannot widen a denied control; the minimum retention wins) and the
   validation that refuses raw secrets (`cargo-test-identity.log`).

Required tests: new-user E2E (above), settings precedence test, secret setup test — all present.

## Gates on fe88d7e

`cargo test -p quansio-server --test identity` 10 passed (incl. the whole flow as
`quansio_app` under RLS, ES256 tamper/expiry, replay revocation, OAuth PKCE);
server crate; fmt/clippy (workspace); `pnpm build`/`typecheck`/desktop vitest 47/47 (18
files)/workspace tests/lint; contract drift CLEAN; validator PASS; `bash scripts/ci/ci.sh`
PASS. Three earlier baseline runs of the same code failed on the surroundings, not the
code: the proto enum lacked the new prefix and the manifest was stale (fixed, 4f992d3);
eslint walked into a concurrent session's worktree (excluded, fe88d7e); a concurrent
dev-stack `reset` restarted the shared test Postgres mid-run (the recorded run pins the
canonical database). Logs kept: `ci-run{1,2,3}-*.log`.
