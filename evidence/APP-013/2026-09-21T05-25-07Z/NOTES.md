# APP-013 — CLI and SDK surface

## What was built (task branch `task/APP-013-cli-sdk`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
`crates/cli` was a verb → path string map with no binary and no dependencies,
`sdk/python` a `.gitkeep`, and the SDK "compatibility test" read the OpenAPI file, never a
server. Ownership decisions are recorded in `evidence/APP-013/RECONCILIATION.md`.

### CLI (`crates/cli`, binary `quansio`)

- `lib.rs` (pure): the argument grammar (`login`, `logout`, `whoami`, `workspaces
  list|use|create`, `threads list`, `messages list|post`, `objectives create`, `runs
  get|timeline|cancel`, `approvals list|show|approve|deny`, `artifacts
  list|versions|download`, `routines list`, `targets list`, `diagnostics bundle`, `api
  get|command`; global `--server`, `--tenant`, `--json`), a credential store scoped to
  `(server origin, tenant)` and written `0600`, the `/v1/` path rule, the digest check,
  token redaction, the diagnostics bundle shape. `--force`, `--bypass-policy`,
  `--no-policy`, `--sudo`, `--admin` are refused by name: there is no flag that widens.
- `main.rs` (transport): `reqwest` over the stored bearer session with one refresh on
  `AUTH_EXPIRED`; every mutation is `POST /v1/commands/<Name>` with a client-minted
  command id; `approvals approve` carries the digest the person previewed
  (`approvals show` prints it); `artifacts download` writes nothing unless the bytes hash
  to the recorded digest; `diagnostics bundle` carries health, the catalogs, the session's
  scope and expiry, and `/v1/me` — never a token; `--json` prints the server's payload
  with tokens redacted; a refusal is printed as the §15 error it is (exit 1; usage exit 2).
- `tests/client_only.rs`: a structural scan — every absolute path literal in the crate is
  under `/v1/`; no database driver, `.env`, worker channel, process spawning or
  trusted-header scope is compiled in.
- `tests/cli_e2e.rs`: the CLI E2E (below).

### SDKs (`sdk/`)

- `scripts/ci/gen_contracts.py --sdk` generates the operation table (`operationId →
  method, path, family`), the read projections and the §15 error codes from the DOMAIN.md
  catalog into `sdk/typescript/src/generated/operations.ts` and
  `sdk/python/quansio_sdk/generated/operations.py`; the contract-drift gate covers both.
- `sdk/typescript`: `QuansioClient` — `command(operationId, params)` typed by the table,
  `get(resource)`, `health`, `me`, the magic-link flow, the §15 error as `ApiError`
  (`isCatalogued`); `tests/client.test.ts` pins the table to the OpenAPI document and the
  catalog; `tests/compat.e2e.test.ts` runs against the current server.
- `sdk/python`: `quansio_sdk` (standard library transport, `Client`, `Credential`,
  `ApiError`) over the same table; `tests/sdk/test_python_sdk_compat.py` runs against the
  current server under the repository's pytest gate.

### Server fixes the CLI E2E found

- `policy/store.rs`: `load_policy_set`, `load_user_rules` and `expire_due` now carry a
  tenant guard. Under the superuser connection every harness uses, row-level security
  does not apply, so a tenant with no policy of its own (an onboarded personal tenant)
  inherited another tenant's policy: the CLI's first `fs.write` was refused by capability
  instead of parking for approval. The guard is structural now, whatever the connection.
- `identity/auth.rs`: an invited user's sign-in accepts the invitation (`invited →
  active` for the tenant and workspace memberships), so the invitee can act at all.

## Acceptance

1. **CLI never bypasses API/runtime policy** — `client_only.rs` (only `/v1/` paths, no
   store/env/worker/process access, no trusted-header scope compiled in); `lib.rs` unit
   tests (bypass flags refused by name, `api get` refuses anything outside `/v1/`,
   `api command` refuses a non-catalogued spelling); `cli_e2e.rs`: an invited viewer's
   `approvals approve` is refused by the server with `SCOPE_FORBIDDEN` /
   `approve_effect` and `/v1/audit` with `SCOPE_FORBIDDEN`, exactly as any client; a
   stale digest is `APPROVAL_SUPERSEDED`; a second decision is `CONFLICT_STATE`; an
   unserved projection is the server's typed `NOT_FOUND`.
2. **SDK compatibility test passes against current server** — `compat.e2e.test.ts` and
   `test_python_sdk_compat.py`: the generated operation ids equal the set the running
   server catalogs (`GET /v1/commands`), the generated projections equal
   `GET /v1/read-projections`, the health counts match, a session comes from the magic
   link through the SDK, a command and a projection go through it, a refusal is a
   catalogued error, and an idempotent replay is reported as such.

## Tests (task list)

| test | where |
|---|---|
| CLI E2E | `crates/cli/tests/cli_e2e.rs` (login from the console link → scoped 0600 session → workspaces → SetPolicy → message → run → approval bound to the previewed digest, stale refused → timeline → artifact download verified → diagnostics bundle without secrets → invited viewer refused by the server → logout) |
| SDK contract fixture | `sdk/typescript/tests/client.test.ts` (table = OpenAPI `/v1/commands/*`; projections and error codes = catalog), `tests/sdk/test_python_sdk_compat.py::test_the_operation_table_is_the_openapi_documents…`, and the drift gate (`gen_contracts.py --check`, `sdk` generator); against the current server: `compat.e2e.test.ts`, `test_the_generated_table_is_what_the_current_server_serves` |
| auth token scope test | `lib.rs::credentials_are_scoped_to_server_and_tenant_and_stored_for_the_owner_only` (a token is never selected for another server or tenant; file mode 0600; redaction); `cli_e2e.rs` (`--tenant`/`--server` mismatch refused before any request; a second user's session lives in its own store; nothing printed contains a JWT) |

## Notes

- `routines list` and `targets list` reach projections no task serves yet (APP-010, the
  EXEC tasks); the CLI reports the server's `NOT_FOUND` rather than inventing a shape.
- `CreateServicePrincipal` and OAuth sign-in are OPS-001/APP-002's; the CLI signs in with
  the magic link the deployable delivers.
- The artifact download case needs the dev MinIO; the E2E says `BLOCKED_EXTERNAL` for
  that case alone when it is unreachable.
