# QA-007 reconciliation — security and privacy adversarial qualification

**Canonical owner.** `tests/security/` (the campaign, its threat manifest and its findings register).
Findings are fixed in the owner that holds the control (named per finding below), in this branch, and
noted on the owner's progress entry.

## Existing code and coverage — `PARTIAL`

`tests/security/test_adversarial_qualification.py` (2026-09-15, reset by the placeholder audit): nine
in-process checks (injection corpus, knowledge lifecycle predicate, skill resolver, CAP-003 validators,
Python redactors, SearchProgram tamper, a structural grep for the stale-generation fence, and — since
QA-004 — the tenant-poisoning case on real PostgreSQL). None attacks the deployable: no test drives the
running `quansio-server`, and the stale-lease case asserts nothing at runtime.

Owners already carry strong per-control suites (approval receipts `crates/server/tests/policy.rs`,
`effects.rs`, `trust_surface.rs`; leases `crates/machine/tests/control.rs`; egress
`crates/machine/tests/egress.rs`, `crates/server/tests/webhooks.rs`; paths `crates/qworkerd/tests/tools.rs`;
secrets `crates/machine/tests/secrets.rs`, `crates/server/tests/observability.rs`; capability
`crates/server/tests/turn_loop.rs`; RLS `crates/server/tests/api.rs`, `schema_bootstrap.rs`; audit
`web_admin_surface.rs`). QA-007 adds the attacker's view over the real HTTP surface and a manifest that
ties every DOSSIER §16 row and every build item to the attacks that cover it.

## Initial findings (triaged before the campaign is written)

| Id | Finding | Severity | Owner |
|---|---|---|---|
| F-1 | The deployable cannot run as the application role: it applies migrations at startup over the DSN it serves with, so it must be the schema owner or a superuser (migration 0001: "Application role: never a superuser, never the table owner"). Every HTTP harness runs as the dev superuser, which bypasses RLS; a compromised process can disable RLS or drop triggers. | P1 | `crates/server/src/composition.rs` (APP-001) |
| F-2 | `DisableSkill` has no authorization check: any member (viewer included) can irreversibly deprecate the tenant's production skills. `InstallSkill` has none either and accepts a caller-chosen `owner`. | P1 | `crates/server/src/api/commands.rs`, `api/knowledge.rs` (APP-011) |
| F-3 | `audit_entries` is not append-only in the database: `quansio_app` holds UPDATE/DELETE and the chain is an unkeyed SHA-256, so a writer with the application's credentials can rewrite an entry and recompute every later hash undetected (DOSSIER §16: "Append-only hash-chained audit"). | P1 | `migrations/`, `crates/server/src/audit/` (OPS-002) |
| F-4 | 26 catalog commands without a handler answer `INTERNAL` rather than a typed refusal. They fail closed. | P3 (recorded) | `crates/server/src/api/commands.rs` |

## Persistent state, contracts, migrations

F-3 needs a forward migration (append-only enforcement on `audit_entries`). F-1 adds one optional
configuration variable (`QUANSIO_MIGRATION_DATABASE_URL`: apply the schema over it, serve over
`QUANSIO_DATABASE_URL`). No contract or event change.

## Real boundaries

The running `quansio-server` binary over HTTP with bearer sessions (`QUANSIO_TEST_SERVER_BIN`,
`QUANSIO_TEST_SEED_BIN`), on PostgreSQL + pgvector (`QUANSIO_TEST_POSTGRES_URL`) with the server serving
as a login role in `quansio_app` (RLS enforced), MinIO for artifact content (`QUANSIO_OBJECT_STORE=1`).
Absent → `BLOCKED_EXTERNAL`.

## Plan

- [ ] F-1: `QUANSIO_MIGRATION_DATABASE_URL`; the campaign server serves as a `quansio_app` login.
- [ ] Harness `tests/security/qa007_harness.py`: scratch database, least-privilege login, server with a
      secret canary and signing key, magic-link sign-in for owner / approver / viewer / outsider tenant.
- [ ] Threat manifest `tests/security/threats.yaml`: every §16 row and build item → attacks (Python HTTP
      tests and named Rust suites), checked against DOSSIER.md and against the test files.
- [ ] Attacks over HTTP: tenant isolation campaign (id guessing on every read, cross-tenant commands,
      forged scope headers, list leakage, the serving role); approval substitution/replay; RBAC and
      capability escalation; secret canaries across every read surface and the server log; SSRF through
      webhook destinations; path traversal (web root, artifact names); malicious skill and content;
      unsafe preview headers; audit tampering.
- [ ] Fix F-2, F-3; findings register with severity and status; negative tests per fix.
- [ ] Mutation checks, Rust owner suites, CI, evidence.
