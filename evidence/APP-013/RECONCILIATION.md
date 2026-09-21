# APP-013 reconciliation (2026-09-21, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `crates/cli` was 78 lines with no dependencies and
no binary (a verb → path string map), `sdk/python` a `.gitkeep`, and the SDK "compatibility
test against the current server" read the OpenAPI file, never a server
(docs/review/2026-09-18-m5-placeholder-audit.md). A stale branch of that era, if any, is
parked as `stale/APP-013-cli-sdk-2026-09-15`.

- **Canonical owners.** `crates/cli/` (the `quansio` binary) and `sdk/` (the TypeScript and
  Python clients), the task's `paths`. Behind them, only the public API v1 (`crates/server/
  src/api`, APP-001; sessions from APP-002; every projection and command the desktop and
  web surfaces already use) and GOV-004's contract generation (`scripts/ci/gen_contracts.py`,
  which already emits the OpenAPI document and the TypeScript types).
- **Existing code, coverage PARTIAL, kept:** the OpenAPI document and its generated
  TypeScript types; `publicApiPath` and the operation-id fixture in `sdk/typescript`; the
  magic-link sign-in whose token the deployable prints on its console (APP-002), which is
  what a terminal user pastes.
- **Gaps.** No binary, no HTTP, no credential store, no verbs; no Python client; no
  generated operation table; no test that talks to a server.
- **Ownership decisions (recorded, not assumed).**
  - *The CLI is a client and nothing else* (DOSSIER.md §4.1): every verb is a `/v1/`
    projection or `POST /v1/commands/<Name>`; the binary holds a bearer session and no
    other credential; it never reads a database, an `.env` or a worker socket; there is no
    flag that widens anything — `quansio api get|command` are constrained to `/v1/` paths
    and catalogued command names. A structural test scans the crate for HTTP paths outside
    `/v1/` and for database/env access, and the E2E shows the server refusing the CLI
    exactly as it refuses the desktop (a viewer's `approvals approve` is
    `SCOPE_FORBIDDEN`; a stale digest is `APPROVAL_SUPERSEDED`).
  - *Credentials are scoped*: the credential file binds each session to `(server origin,
    tenant, user)`; `--server`/`--tenant` that do not match the stored session are refused
    before any call; the file is written `0600`; tokens are never printed (`--json`
    redacts them; the diagnostics bundle carries only their scope and expiry).
  - *SDKs are generated where the contract is, hand-written where the transport is*:
    `gen_contracts.py` gains an `sdk` generator that emits the operation table
    (`operationId → method, path, family`) and the read-projection list from the OpenAPI
    document for both languages (`sdk/typescript/src/generated/operations.ts`,
    `sdk/python/quansio_sdk/generated/operations.py`); the drift gate covers them. The
    thin clients (`QuansioClient` in TypeScript, `quansio_sdk.Client` in Python, standard
    library only) address operations by id and carry the §15 error as a typed error.
  - *Compatibility is measured against the running server*: the SDK compatibility tests
    spawn `quansio-server` on a scratch database and assert that every generated operation
    id is in `GET /v1/commands` and every generated projection in
    `GET /v1/read-projections`, then exercise a command and a projection through the SDK.
    Blocked, not skipped, without the dev stack or the built binaries.
- **Persistent state touched:** none on the server. The CLI writes its own credential file
  (`$QUANSIO_CLI_HOME` or `~/.config/quansio/credentials.json`).
- **Contracts:** no DOMAIN.md change. **External effects / real boundaries / migrations:** none.
- **Verbs that reach an unserved projection** (`routines`, `targets`: APP-010, EXEC tasks)
  report the server's typed `NOT_FOUND` — the CLI invents nothing.

## Plan

1. `crates/cli`: `lib.rs` (argument grammar, credential store, `/v1/` path rule, digest
   check, output shaping) and `main.rs` (`quansio` over `reqwest`): `login`, `logout`,
   `whoami`, `workspaces list|use|create`, `threads list`, `messages list|post`,
   `objectives create`, `runs get|timeline|cancel`, `approvals list|show|approve|deny`,
   `artifacts list|versions|download`, `routines list`, `targets list`, `diagnostics
   bundle`, `api get|command`, global `--json`, `--server`, `--tenant`.
2. `sdk/`: generator step; TypeScript `QuansioClient`; Python `quansio_sdk` package.
3. Tests: `crates/cli` unit tests (grammar, store scope and permissions, `/v1` rule),
   `crates/cli/tests/cli_e2e.rs` (CLI E2E: magic link from the console → session →
   workspace → message → run → approval bound to the previewed digest → artifact download
   verified → diagnostics bundle → viewer refused), `sdk/typescript/tests/compat.e2e.test.ts`
   and `tests/sdk/test_python_sdk_compat.py` (SDK contract fixture against the current
   server), and the auth token scope test (credential file binding, tenant mismatch refused,
   redaction).
