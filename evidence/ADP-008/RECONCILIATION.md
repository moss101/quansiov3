# ADP-008 reconciliation (2026-10-07, agent:adp-008)

Tool visibility split, content-hash pack revisions and redacted install reports. Dependencies RUN-011, APP-003 and INT-012 are
PASS; `real_boundary` is false (D-019: no new persistent store, no new Go, no second registry or install path).

## Canonical owners (match the task's `paths` where they name one)

| Concern | Owner | Notes |
|---|---|---|
| Tool declarations, the registry, the model-facing filter, caller checks | `crates/tools`, `config/tools.yaml` | `exposed_to` / `exposed_names` / `is_exposed` are already the one filter the runtime uses for the model's tool list; they gain the `model_visible` rule. `plan_call` gains the caller rule. |
| Dispatch of a proposed call, its rejection record | `crates/server/src/runtime/turn_loop/dispatch.rs` (RUN-011) | `reject()` already writes a `tool_calls` row and a `tool.rejected` event; a model call to a host-only name goes down that path. |
| The model's tool schemas (what a provider is offered) | `python/intelligence/model_gateway/tooling.py` | reads `config/tools.yaml` read-only; must not offer a tool the registry hides. |
| What a recording compile is told the registry offers | `crates/server/src/control/skills/recording/proposal.rs` | used `callable_names()` (every declaration): now the model-visible set. |
| Pack content, verification, compilation (revision, quarantine, tool resolution, credential scan) | `python/intelligence/capability_compiler/` (CAP-004/CAP-005) | new `install.py` beside `compiler.py` / `lifecycle.py`; reuses `scan_for_credentials`, `CompilerError` rules and the lifecycle's digest-pinned history and deterministic rollback. |
| Pack revisions, activation, retention, rollback, install reports | `crates/server/src/control/packs/` (the path CAP-005 named; it did not exist) over the 0001 tables `capability_packs` / `capability_pack_versions` | one install path: `InstallPack`; `RollbackPack` (catalog command, no handler until now). |
| Wiring only | `api/commands.rs`, `api/packs.rs`, `api/mod.rs`, `api/error.rs`, `intelligence/mod.rs`, `composition.rs` | handlers call the owner. |

## Existing code and its coverage

* `ToolDeclaration`, `ToolRegistry`, `plan_call`: PARTIAL. Every declaration is "model-visible" implicitly; there is no
  host caller and no way to say a tool is not for the model. `exposed_to` already filters by projection grants only.
* `RegistryToolSchemaSource.load`: PARTIAL. Loads every declaration, so a host-only tool would be offered to a provider.
* `tool_summaries(registry)` (ADP-003): PARTIAL. Told the plane every declared name.
* Packs: `packs/capabilities/*/pack.yaml` (CAP-007) carry a governance record (quarantine, review, normalization, evaluation,
  approved) and `provenance.digest` = sha256 of the canonical manifest without that record (`scripts/dev/pack_records.py`).
  `capability_packs` / `capability_pack_versions` exist in migration 0001 with RLS but nothing writes them. Python
  `PackLifecycle` (CAP-005) is in-memory: digest-pinned `published` list, `rollback()` pops to the previous. No server install
  path exists: `SubmitPack`/`QualifyPack`/`PublishPack`/`RollbackPack` are catalog names with no handler. Classification:
  GENUINE_GAP for the server side, PARTIAL for the plane side.
* Standalone `InstallSkill`: a DRAFT version through `SkillStore`, immutable per `(skill, semver)`, climbs the §11.5 ladder.
  Untouched (see deferrals).
* Retention: the repository's mechanism is the lazy in-transaction sweep (ADP-007 `routine_hook_deliveries`, ADP-003 recording
  expiry): no timer, no daemon. Install reports use it.

## Audit of `config/tools.yaml` (44 declarations)

Every existing tool is something a model proposes to do. None is a host control: `webdev.preview_open/renew/close/suggestions`
act on a scope the *run* opened and the model reads the person's suggestions as data (DOMAIN 10.4 F); `video.render/inspect`
are supervised jobs whose cancel is the call's own cancellation; approval resolution is the `ApproveEffect`/`DenyEffect`
commands, not a tool; there is no widget tool. They keep the defaults (`model_visible: true`, `host_callable: false`).
One genuinely host-only tool is added so the split is exercised by the shipped registry, not only by fixtures:
`webdev.preview_scopes` lists the live preview scopes of the acting member for the person's own preview panel.

## Persistent state touched (migration 0029, additive)

* `tool_calls.caller` (`model` | `host`, default `model`): who proposed the call, so a parked host call resumes as a host call
  and a rejected model attempt on a host-only name is recorded as the model's.
* `capability_pack_versions`: `revision`, `manifest`, `files`, `activated_at`, `retired_at`; unique `(pack_id, revision)`;
  the unique `(pack_id, semver)` of 0001 is dropped (a revision, not a semver, is the identity).
* `pack_install_reports` (RLS enabled and forced, `quansio_app` grant): one row per install attempt, keyed by
  `(tenant_id, command id)`.
No other table, store or service.

## Contracts and events (DOMAIN.md changed in the same commits)

7.5 (tool visibility), 11.6 (revisions, install reports), 14 (`InstallPack`, `RollbackPack` params, `/packs`), 15 (tool and
pack codes), glossary; events `pack.installed`, `pack.install_failed`, `pack.rolled_back`. Proto: `CompilePack` on the
IntelligenceGateway (the plane compiles; the server commits).

## External effects and classes

None new. Installing a pack changes what may be used tenant-wide: a tenant administrator's command (`ManageTenant`), like
`InstallSkill`. A pack names tools; it grants nothing and cannot name a tool the model cannot see.

## Capabilities and approvals

Capability only narrows: a host-only tool is excluded after grant coverage, so no grant, projection layer, pack or skill makes
it model-visible. A pack manifest naming one fails `compile`.

## Real boundaries

None required (`real_boundary: false`). Tests use a private PostgreSQL database, the real Python plane (a child process over
gRPC) and the real dispatch path. `QUANSIO_TEST_POSTGRES_URL`; `uv`.

## Rollback and recovery impact

Migration 0029 is additive. Activation is one transaction that moves the active pointer; a crash before it leaves the previous
revision active, and a `running` report left behind is closed as `failed` / `retained_previous` by the next install or read.
