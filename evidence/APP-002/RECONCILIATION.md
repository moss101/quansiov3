# APP-002 reconciliation (2026-09-19, agent:claude-opus-5)

The 2026-09-15 record was partial (docs/review/2026-09-18-m5-placeholder-audit.md): a real
`IdentityStore` (onboarding of a personal tenant, layered settings) that nothing called,
OAuth/magic link recorded as names only, no session, no keychain, a 35-line onboarding plan
object on the desktop.

- **Canonical owner:** `crates/server/src/control/identity/` (server side) and
  `apps/desktop/src/onboarding/` (client flow). Matches `paths`.
- **Existing code, coverage PARTIAL, kept:** `IdentityStore::onboard_email` (personal tenant,
  default workspace, memberships, `workspace.created` event), `effective_for_workspace`,
  `settings.rs` (deterministic, most-restrictive merge), `enterprise.rs` (OPS-001's SSO/SCIM
  shapes, untouched), migration 0012 (settings columns).
- **Persistent state touched (new, forward-safe migration 0013):** `auth_challenges`
  (magic-link tokens, hashed), `oauth_states` (PKCE verifier + state, expiring),
  `sessions` (`ses_`: user, tenant, refresh-token hash, rotation family, revocation).
  `users.auth_methods` gains the concrete method that signed the user up. No new prefix
  outside DOMAIN.md except `ses_`, added to DOMAIN.md §1 in the same commit.
- **Contracts:** DOMAIN.md §2 — passwordless email link + OAuth (Google, Microsoft, GitHub);
  access token ES256 JWT ≤ 15 min; opaque rotating refresh token, OS keychain on desktop.
  §14 identity commands implemented here: `CreateWorkspace`, `UpdateWorkspace`,
  `UpdateUserSettings`. New unauthenticated auth routes under `/v1/auth/*` and `GET /v1/me`
  are added to `schemas/openapi/public-api-v1.yaml` (public API v1 is additive-only).
- **Scope resolution:** `Authorization: Bearer <access JWT>` becomes the session; the
  `x-quansio-tenant`/`x-quansio-user` headers remain only in an explicit trusted-headers
  mode (`ApiState::trust_scope_headers`, on for in-process tests, off in the deployable
  unless `QUANSIO_TRUST_SCOPE_HEADERS=1` for dev harnesses).
- **External effects:** OAuth code exchange and userinfo are outbound HTTPS to the provider
  (real boundary, not required for this task: `real_boundary: false`; unconfigured providers
  fail closed with `PROVIDER_UNAVAILABLE`). Magic-link delivery is an email effect (OD-009,
  APP-010's channel); until it exists the delivery port has a console sink for
  development, which the new-user E2E reads exactly as a developer would.
- **Secrets:** the ES256 signing key comes from `QUANSIO_AUTH_SIGNING_KEY_PKCS8_B64`
  (secret custody) or is generated per process with a logged warning; refresh tokens and
  magic-link tokens are stored hashed (SHA-256); provider credentials stay `sec_` handles.
- **Capabilities/approvals:** none (identity.change approval semantics are policy, RUN-*).
- **Rollback/recovery:** migration adds tables only; a session family revocation is the
  recovery for a leaked refresh token (reuse detection revokes the family).

## Plan

1. Migration 0013; `identity/auth.rs`: signing key, JWT mint/verify, challenge store,
   session store with rotation + reuse detection, find-or-onboard by email.
2. `identity/oauth.rs`: provider config from `config/auth.yaml`, PKCE start, code exchange
   + userinfo over reqwest, provider adapters for Google/Microsoft/GitHub.
3. API: `/v1/auth/magic-link/{start,complete}`, `/v1/auth/oauth/:provider/{start,complete}`,
   `/v1/auth/refresh`, `/v1/auth/sign-out`, `/v1/me`; Bearer scope; commands
   `CreateWorkspace`/`UpdateWorkspace`/`UpdateUserSettings` (secrets as `sec_` handles;
   effective settings and restart/reindex effects reported).
4. Desktop: onboarding flow over the bridge (magic link / OAuth → me → execution target +
   security defaults), refresh token in the OS keychain via Electron `safeStorage`,
   Bearer on every request, refresh on `AUTH_EXPIRED`.
5. Tests: Rust identity suite (sessions, rotation, reuse, expiry, precedence, secret setup,
   Bearer required); desktop unit tests; new-user E2E against the real binary with no seed
   and no config file.
