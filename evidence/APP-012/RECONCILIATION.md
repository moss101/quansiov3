# APP-012 reconciliation (2026-09-20, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `apps/web` was 125 lines (a surface list and a
`canOpen(surface, role)` predicate) with no app, no client, no page, nothing served
(docs/review/2026-09-18-m5-placeholder-audit.md). The stale branch of that era is parked
as `stale/APP-012-web-admin-2026-09-15`.

- **Canonical owners.** The surface is `apps/web/` (the task's `paths`). Behind it, the
  public API (`crates/server/src/api`, APP-001) and the owners it calls: identity and
  memberships (`control/identity`, APP-002), policies and RBAC (`policy/`, RUN-006:
  `rbac::check(ActorRoles, ActionFamily)` already encodes DOMAIN.md §2's role semantics
  and is applied on the effect-execution path), usage (`usage/`, OPS-004: a pure
  projection over RuntimeEvents), connectors (`control/connectors`, EXEC-011), the audit
  log (`audit/`, OPS-002: DOMAIN.md §16's hash-chained `audit_entries`), approvals and
  effects (APP-006's projections and commands), the WorkGraph and artifacts (APP-005,
  APP-008).
- **Existing code, coverage PARTIAL, kept:** every projection and command the desktop
  surfaces use; sessions (`/v1/auth/*`, `/v1/me` with the user's tenant role and
  workspace roles); `rbac::check`; `PolicyStore::load_policy_set`; `UsageProjection`.
- **Gaps.** No RBAC is applied at the API: `ApproveEffect` needs an acting user but not
  an approver; there is no admin-only projection or command at all. `GET /v1/audit`,
  `/v1/usage`, `/v1/connectors` (catalogued) are unserved; `SetPolicy`, `InviteMember`,
  `UpdateMembership`, `RemoveMember` (catalogued) are unimplemented; nothing writes
  `audit_entries` (OPS-002's `AuditLog` is an in-memory value model). `quansio-server`
  serves no static assets. The harness seeds create no memberships, so a seeded user
  holds no role at all.
- **Ownership decisions (recorded, not assumed).**
  - *Authorization is the server's, once, for every client.* `api/rbac.rs` resolves the
    session user's tenant role and workspace role from the membership rows and applies
    RUN-006's `rbac::check`; a refusal is `SCOPE_FORBIDDEN`. Admin projections
    (`/v1/audit`, `/v1/members`, `/v1/policies`, `/v1/connectors` → `ManageWorkspace` /
    `ManageTenant`; `/v1/usage` → `ViewUsage`), admin commands (`SetPolicy`,
    `InviteMember`, `UpdateMembership`, `RemoveMember`) and the trust decisions
    (`ApproveEffect`, `DenyEffect` → `ApproveEffect`) are gated. Read projections stay as
    they are (a membership check there is APP-002/OPS-001's call); the web app hides
    nothing it cannot enforce — the server refuses, the page shows the refusal.
  - *Memberships* are APP-002's identity owner: `InviteMember` (an existing user by
    email, or a new user provisioned `invited` the way onboarding provisions one),
    `UpdateMembership`, `RemoveMember`, `GET /v1/members` live in `control/identity`.
  - *Policies*: `SetPolicy` and `GET /v1/policies` live in RUN-006's `PolicyStore`
    (a versioned upsert of the tenant or workspace policy row; the typed rule
    vocabulary is the store's).
  - *Audit*: a durable `AuditStore` in `crates/server/src/audit/` (OPS-002's home) —
    `append_in(tx, …)` chains `prev_hash`/`hash` per tenant inside the owner's own
    transaction, `list` and `verify` serve the page. The admin commands above and the
    trust decisions append their AuditEntry in the same transaction as their mutation.
    OPS-002 extends this (retention, export, deletion audit); this task adds the store
    and the entries its pages show.
  - *Usage*: `GET /v1/usage` rebuilds OPS-004's projection from the tenant's
    RuntimeEvents on read; no table is materialised.
  - *Static assets*: `QUANSIO_WEB_ROOT` makes `quansio-server` serve the built web app
    under `/app/` (index fallback for client routes, `nosniff`, a locked CSP, no
    directory listing, no path escape). A CDN can serve the same directory.
- **Persistent state touched:** `tenant_memberships`, `workspace_memberships`, `users`
  (invited), `policies` (versioned rows), `audit_entries`. No migration.
- **Contracts (DOMAIN.md §14):** `GET /v1/members` and `GET /v1/policies` are added to
  the read-projection row (additive; contracts regenerated). Everything else is already
  catalogued.
- **External effects / real boundaries / migrations:** none. The web app uses bearer
  sessions from `/v1/auth/*` in production and never a host API; the harness uses the
  same trusted-header mode the desktop E2Es use.

## Plan

1. Server: `api/rbac.rs`; membership commands and projection; `SetPolicy` and
   `/v1/policies`; `AuditStore` and `/v1/audit`; `/v1/usage`; `/v1/connectors`; gate
   the trust decisions; `api/web.rs` static serving; seeds create memberships.
2. Web app (`apps/web/src`): `client.ts` (bearer or trusted headers; the same v1
   paths and error shape), `rbac.ts` (server roles from `/v1/me`, the page-side
   mirror of `rbac::check` used only to decide what to *offer*), decoders for each
   page, `App.tsx` with responsive navigation (`layoutFor(width)`), pages: work review,
   approvals (bound decisions as the desktop binds them), artifacts, policies,
   connectors, users, capabilities, audit, usage; esbuild bundle to `dist/app`.
3. Tests: `web RBAC E2E` (a viewer is refused every admin page and command by the
   server; an owner is served; the page offers only what the role allows), `approval E2E`
   (an approver decides through the web client; a viewer's decision is refused), and
   `responsive smoke` (layout breakpoints; the served `/app/` bundle carries the viewport
   meta and media queries; the server serves it with the right headers).
