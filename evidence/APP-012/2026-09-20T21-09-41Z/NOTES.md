# APP-012 — Web administration and review surface

## What was built (task branch `task/APP-012-web-admin`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
125 lines across six files — a surface list and a `canOpen(surface, role)` predicate —
with no app, no client, no page and nothing served. The stale branch of that era is
parked as `stale/APP-012-web-admin-2026-09-15`. The ownership decisions (authorization
decided by the server once for every client; memberships in the identity owner; policies
in the policy store; a durable audit chain in OPS-002's home; usage rebuilt from events;
static assets served by the deployable) are recorded in `evidence/APP-012/RECONCILIATION.md`.

### Server

- `crates/server/src/api/rbac.rs` — `Authorization::resolve` reads the session user's
  tenant role (`tenant_memberships`) and workspace role (`workspace_memberships`, `active`
  only) and `require(action)` applies RUN-006's `rbac::check`; a refusal is
  `SCOPE_FORBIDDEN` with `details {kind: rbac, action, reason, tenant_role,
  workspace_role}`. No membership is no role, and no role is a denial.
- `crates/server/src/api/admin.rs` — `GET /v1/members`, `/v1/policies`, `/v1/connectors`
  (manage_workspace), `/v1/usage` (view_usage: OPS-004's `UsageProjection` rebuilt from
  the tenant's RuntimeEvents on read, totals per meter), `/v1/audit` (manage_tenant: the
  chain verified from genesis on every read); commands `InviteMember` (manage_tenant, or
  manage_workspace for a plain member into a workspace), `UpdateMembership` /
  `RemoveMember` (by the membership's scope), `SetPolicy` (manage_tenant for the tenant
  policy, manage_workspace for a workspace's). `ApproveEffect` and `DenyEffect` are gated
  by `approve_effect` on the request's workspace (`api/trust.rs`).
- `crates/server/src/control/identity/members.rs` — `list_members`, `invite_member` (an
  existing user by email, or a new one provisioned `invited` the way onboarding
  provisions; a removed membership is revived; an active one is `CONFLICT_STATE`),
  `update_membership` (role in the scope's vocabulary, `active | suspended`),
  `remove_member` (a `tm_` removal removes the workspace memberships too); the last active
  owner cannot be demoted, suspended or removed; every change commits its `member.*`
  RuntimeEvent and its AuditEntry in one transaction.
- `crates/server/src/policy/store.rs` — `list_policies`, `set_policy` (a versioned upsert
  of the tenant or workspace row; rules parsed with the typed vocabulary before anything
  is written; `policy.updated` and the AuditEntry in the same transaction); grants and
  human denials append their AuditEntry inside the store's own transaction.
- `crates/server/src/audit/store.rs` — the durable per-tenant hash chain over
  `audit_entries`: `append_in(tx, draft)` under a transaction-scoped advisory lock,
  `prev_hash` = the tail's `hash`, `hash` over the canonical form the in-memory model
  already defined; `list`; `verify` recomputes the chain and names the first broken row.
- `crates/server/src/api/web.rs` + `QUANSIO_WEB_ROOT` — the built web app under `/app/`:
  index fallback for client routes, a locked CSP (`default-src 'self'`, no inline
  script, `frame-ancestors 'none'`), `nosniff`, `X-Frame-Options: DENY`, `no-cache`
  (assets are not content-hashed), a path that cannot climb (`..`, absolute and prefix
  components are refused before any lookup; a directory is never listed), a 16 MiB
  ceiling. `/v1/me` answers a trusted-header scope (development harnesses only).
- Seeds create memberships: `quansio-dev-seed` makes the seeded user the tenant owner and
  workspace admin and `--member <usr>:<tenant role>:<workspace role>` adds other roles;
  `tests/common::seed_tenant` does the same. DOMAIN.md §14 gains `/members` and
  `/policies` (contracts regenerated).

### Web (`apps/web/src`)

- `client.ts` — `WebClient`: `/v1/` paths only, `POST /v1/commands/<Name>` with a
  client-minted ULID-shaped `command_id`, the §15 error body as `ApiError`
  (`isRbacRefusal`); a bearer session or the harness's trusted headers.
- `session.ts` — `WebSession`: magic-link start/complete, the session kept for the tab
  (`sessionStorage`), one refresh on `AUTH_EXPIRED`, sign-out forgets it.
- `rbac.ts` — the page-side mirror of `rbac::check` (`allows`), roles read from `/v1/me`
  (`rolesFromMe`); `surfaces.ts` — every surface names the action the server gates it
  by, its projections and commands (pinned to the catalog by the unit test); `offered(roles)`.
- `review/` — decoders and validated command builders: approvals (`decide` bound to the
  previewed digest: a changed digest, a status other than `requested` or a passed expiry
  is refused before the command), work review (`needsAttention`), artifacts (content is
  linked as an attachment, never rendered), members/policies/connectors/audit/usage,
  `invite`, `updateMembership`, `removeMember` (last-owner guard), `setPolicy`.
- `layout.ts` — `layoutFor(width)`: drawer < 720px, rail < 1080px, sidebar above;
  `app.css` carries the same breakpoints as media queries.
- `app/` — `App.tsx` (sign-in, workspace picker, navigation from `offered(roles)`, the
  page for the route, the drawer on phones), `pages.tsx` (one page per surface; a
  `SCOPE_FORBIDDEN` refusal renders as the refusal), `router.ts` (`/app/<surface>[/<id>]`),
  `strings.ts`, `main.tsx`, `index.html`, `app.css`; `scripts/bundle.mjs` → `dist/web`.

## Acceptance

1. **Web uses same public contracts and authorization rules as desktop** — the web client
   addresses only `/v1/` (`publicPath` refuses anything else), sends the same command
   envelope and decodes the same §15 error body; every surface's projections and commands
   are pinned to `schemas/catalog/` by `tests/rbac.test.ts`; and authorization is decided
   by the server for both clients: `web_admin_surface.rs` and `web.e2e.test.ts` show the
   same `SCOPE_FORBIDDEN` for the same roles whether the request is a page's or a test's,
   and the desktop's trust E2E still passes under the new `approve_effect` gate because
   its seeded user is a workspace admin.
2. **Admin-only pages enforce server RBAC** — `web.e2e.test.ts` (web RBAC E2E): a viewer
   is refused `/v1/members`, `/v1/policies`, `/v1/connectors`, `/v1/usage`, `/v1/audit`,
   `SetPolicy` and `InviteMember` by the server with typed `rbac` details, whatever the
   page addressed; the page offers the viewer the three review surfaces only; an owner is
   served and invites, activates and removes a member and versions the tenant policy.
   `web_admin_surface.rs` covers every role (viewer, approver, billing, no membership,
   invited-not-active, owner), the last-owner guard, the tenant-vs-workspace scope of a
   membership change, and the audit chain (order, actors, genesis, a tampered row).

## Tests (task list)

| test | where |
|---|---|
| web RBAC E2E | `apps/web/tests/web.e2e.test.ts` "web RBAC E2E…"; `crates/server/tests/web_admin_surface.rs::admin_pages_and_commands_are_decided_by_the_servers_rbac_for_every_role`; `apps/web/tests/rbac.test.ts` |
| approval E2E | `web.e2e.test.ts` "approval E2E…" (viewer refused, approver bound to the preview, stale digest superseded, audited under the approver); `web_admin_surface.rs` (approve/deny gate); `tests/review.test.ts` |
| responsive smoke | `web.e2e.test.ts` "responsive smoke…" (served bundle: viewport meta, CSP, breakpoints, no external script); `tests/layout.test.ts`; `web_admin_surface.rs::the_web_app_is_served_under_app…` |

Verified by hand in the built-in browser against a seeded server: the owner sees all nine
surfaces and the Users/Policies/Audit pages; a viewer opening `/app/audit-and-privacy`
directly sees the server's refusal and, on a 375px viewport, the drawer offers the three
review surfaces; an approver approves a parked `fs.write` from the Approvals page and the
owner's Audit page shows the granted decision under the approver's identity.

## Notes

- Read projections keep their existing access (a signed-in user of the tenant); gating
  them by workspace membership is APP-002/OPS-001's call and is noted in the
  reconciliation.
- `CreateServicePrincipal`, `RevokeServicePrincipal`, connector commands and the
  Admin/ops kill switches remain unimplemented (OPS-001, EXEC-011, OPS-008); the
  connectors page is read-only.
- OPS-002's placeholder-era `AuditLog` value model stays beside the durable store; its
  own re-verification owns retention, export and deletion audit.
