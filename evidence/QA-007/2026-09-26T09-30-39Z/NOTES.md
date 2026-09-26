# QA-007 evidence — security and privacy adversarial qualification

**Campaign.** `tests/security/test_qa007_campaign.py` attacks the running `quansio-server` over HTTP
with real bearer sessions (owner, approver and viewer of tenant A; the owner of tenant B), the server
serving as a `quansio_app` login — no superuser, no table ownership, no BYPASSRLS — so forced
row-level security applies to everything it does (`qa007_harness.py`). `tests/security/threats.yaml`
maps every DOSSIER §16 threat and every QA-007 build item to its attacks (the campaign plus the
owners' Rust suites for surfaces HTTP does not reach) and holds the findings register;
`test_threat_manifest.py` checks it against DOSSIER.md, the task and the test files.

## Findings

| Id | Severity | Finding | Status |
|---|---|---|---|
| F-1 | P1 | The deployable migrated over the DSN it serves with, so it ran as the schema owner or a superuser; RLS was never exercised through HTTP. | fixed — `QUANSIO_MIGRATION_DATABASE_URL` |
| F-2 | P1 | `InstallSkill`/`DisableSkill` had no authorization: a viewer installed under another owner's name and could irreversibly deprecate production skills. | fixed — `ManageTenant` (DOMAIN §11.5) |
| F-3 | P1 | `audit_entries` was UPDATE/DELETE-able by the application role; the unkeyed chain could be rewritten and recomputed. | fixed — migration 0022 |
| F-4 | P3 | 26 handler-less catalog commands answer `INTERNAL` rather than a typed refusal (fail closed). | open (recorded) |
| F-5 | P2 | `UpdateUserSettings` failed for everyone under the application role and echoed the database's message. | fixed |
| F-6 | P2 | Content relabelling was case-sensitive and list-based: `TEXT/HTML`, `IMAGE/SVG+XML`, `text/xsl`, `*+xml` were served as themselves. | fixed |
| F-7 | P2 | A command could name another tenant's `workspace_id`; rows landed in the caller's tenant against the foreign workspace (existence oracle, contradictory pair). | fixed — command-surface check + migration 0023 composite keys |
| F-8 | P3 | Webhook creation accepts numeric IPv4 shorthands / loopback-resolving names; every delivery resolves and refuses them (0 connections to a local listener). | open (recorded) |

No P0/P1 finding is open. Side effect of F-7's composite keys: `scripts/dev/seed_test_database.py` had
been placing every workspace in the first tenant, so multi-tenant integration tests wrote
contradictory rows; it now accepts `ws_…@tn_…`.

## Mutation checks

Each fix undone, the deployable rebuilt, the campaign re-run: F-1 (serve as owner), F-2 (DisableSkill
and InstallSkill with the authorization result discarded), F-3 (audit grants and trigger removed),
F-5 (settings without user context), F-6 (case-sensitive relabel), F-7 (guard and composite keys
removed), and RLS disabled on tenant tables — 8 of 8 killed. Two first-draft F-2 mutants (the family
swapped to `Read` with no workspace) survived because they still refused the viewer; they are
equivalent mutants, recorded in `mutation-checks.log`.

## What held (attacks with typed refusals and positive controls)

- Tenant isolation campaign: 42 reads across 21 routes in both directions carried nothing of the
  victim's; 9 cross-tenant commands refused with the victim unchanged; forged scope headers ignored;
  the serving role reads no row without a tenant context.
- Approval substitution/replay: viewer, other tenant, substituted and malformed digests refused with
  nothing granted; the grant settles once under one consumed receipt; a replay returns the recorded
  result; a new decision is `CONFLICT_STATE`; a reused command id with other params is
  `CONFLICT_IDEMPOTENCY_MISMATCH`.
- Escalation: 16 admin commands and 9 admin reads refused (`SCOPE_FORBIDDEN`) for viewer and approver;
  a `Conversation`-scoped service principal confined to its family.
- SSRF: 16 internal/metadata literals refused at creation (`EGRESS_DENIED`), 4 malformed schemes or
  credentialed authorities refused (`VALIDATION_SCHEMA`), 4 disguised loopback destinations refused at
  every delivery with zero connections to a local listener.
- Path traversal: 10 traversal spellings under `/app/` never reached the file beside the web root;
  artifact names are flattened; header injection through a title is impossible.
- Unsafe preview: 9 active media types served `text/plain`, `attachment`, `nosniff`; the web app's CSP
  has no `unsafe-eval` and no inline script.
- Malicious skill: a draft with injection instructions and escalated needs cannot jump to `active`, is
  never an input of a run's capability projection, and grants nothing the policy withholds.
- Audit: UPDATE, DELETE, TRUNCATE and disabling the guard are refused for the serving role; the chain
  holds.
- Secret canaries: 15 issued or planted secrets (four access and four refresh tokens, the
  service-principal key, the webhook signing secret, the serving role's password, the secrets master
  key, the metrics token, and the canary — which is also inside the approval signing key and the
  provider credential in the server's environment) absent from 14 read surfaces, `/metrics` and the
  server log.
