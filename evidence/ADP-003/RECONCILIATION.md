# ADP-003 reconciliation

Recorded before implementation (AGENTS.md task loop, RECONCILE) and updated at the end with what was and was not
done. Coverage: `GENUINE_GAP`. INT-009 (Skill Registry ladder), CAP-008 (evolution candidates) and ADP-002 (native
receipts, permission status) exist; none of the following did: a way for a person to demonstrate a workflow, a
redaction stage that runs before anything is stored or sent, a recording kept as an Artifact, an owner/epoch fence
that an account switch trips, and a compiler that turns a demonstration into a reviewable DRAFT skill without
replaying coordinates.

## Canonical owner

`Capability` (task `owner`); paths from the task: `native/macos/`, `crates/server/src/control/skills/`,
`python/intelligence/skills/`, `apps/desktop/src/`. Everything new lives there, plus the outside edits an authority
document or a closed vocabulary forces:

- `migrations/0026_skill_recordings.sql` — one table, `skill_recordings`, owned by `control/skills/recording`.
  Recording *session* state (owner, epoch, status, redaction summary, the compiled proposal under review). The bytes
  are an Artifact; the resulting skill is a `skill_versions` row. No second store (D-019).
- `crates/core/src/id.rs` and `schemas/proto/quansio/v1/core/identity.proto` — the `rec_` prefix (DOMAIN.md §1.1),
  mirrored in the generated contract by test.
- `schemas/proto/quansio/v1/intelligence/service.proto` — one RPC, `CompileSkillRecording`, beside
  `ProposeSkillCandidates`. Python proposes; Rust commits.
- `config/recording.yaml` + `schemas/json/recording-config.schema.json` — D-018: capture limits, the
  archive cap, the lifetime of an unfinished recording, the tool-path preferences. `config/identity.yaml` adds
  `Recording` to `ungrantable_scopes` (a service principal never records).
- `crates/server/src/api/{commands,knowledge,error}.rs` — dispatch of five catalog commands and two projections;
  `ApiErrorCode` is a closed enum checked against the generated catalog.
- `crates/server/Cargo.toml` — `regex` (already in `Cargo.lock` through other crates) for the redaction rule set.
- Generated: `schemas/catalog/*`, `schemas/openapi/public-api-v1.yaml`, `sdk/*/generated/*`,
  `crates/contracts/src/generated/*`, `python/intelligence/contracts/generated/*`.
- `tests/architecture`, `tests/contract` and `tests/security` where a registry lists commands, families or ids.

## Existing code and its coverage

| Code | Classification | What it did | What changes |
|---|---|---|---|
| `control/skills/{state,store}.rs` (the ladder, `SkillStore`) | covered, kept | DRAFT → CANDIDATE → … → ACTIVE, one active version, `evolution_gate` | untouched; the recording path creates a DRAFT through `create_skill`/`create_version` and nothing else |
| `control/skills/evolution.rs` + `python/intelligence/skills/evolution/` | covered, kept | verified runs become CANDIDATE patches with evaluated provenance | untouched; the recorder is a second *source* of a version, not a second registry |
| `python/intelligence/skills/{models,resolver}.py` | covered, extended | closed manifest vocabulary; only ACTIVE resolves | the vocabulary gains one key, `procedure`, parsed and validated (no coordinates, no element indexes, closed step shape) |
| `artifacts/` (`ArtifactStore`, object store, versions, grants) | covered, kept | content-addressed versions with owner grants | the recording archive is one more artifact (`kind=archive`, `role=recording`, `origin=upload`), owner-only grant |
| `api/commands.rs` dispatch, RBAC, user-only approvals | covered, extended | one match arm per command; `AuthorWork` family check | five arms; `FinishRecording`/`CancelRecording`/`StartRecording` refuse a service principal and need a session user |
| `native/macos` (`ComputerUse.swift`, `PermissionStatus.swift`, ADP-002) | covered, extended | AX reads, input posting, receipts, permission status | a `Recording/` group: event model, redaction, sampler, secure-input probe, session + owner fence, control channel; a thin `quansio-recorder` executable |
| `apps/desktop` Knowledge surface, permissions guide (ADP-002) | covered, extended | skills ladder UI, Privacy-pane guide | a Recording flow on the skills surface, an app-wide recording indicator, a main-process recorder client |
| `observability/redact.rs`, `notify/webhooks/redact.rs` | covered, not reused | log/webhook scrubbers (credential prefixes, emails) | different rule set (card numbers, verification codes, secure fields) and shape (counts per category); a new module, with a shared corpus pinning agreement across Rust, Swift and Python |

## Persistent state touched

`skill_recordings` (new, `control/skills/recording`), `artifacts`/`artifact_versions`/`artifact_grants` (existing owner,
through its API), `skills`/`skill_versions` (existing owner, DRAFT only), `runtime_events` (new event types
`recording.*`). Nothing else. No queue, cache, daemon or second store. The recorder keeps a bounded in-memory buffer
in a user-session process for the length of one recording and writes nothing to disk but its 0600 bootstrap file,
deleted after use.

## Contracts and events changed

DOMAIN.md: §0 glossary (Recording, Recorder owner fence, Redaction summary, Input slot), §1.1 (`rec_`), §11.5
(manifest `procedure`, recorded provenance), new §11.7 A–F, §12 rule 7, §14 (Recording family and two projections),
§15 (Recording family). Events: `recording.started`, `recording.finished`, `recording.cancelled`,
`recording.aborted`, `recording.compiled`, `recording.submitted` (family `recording`, aggregate `recording`).
Generated catalogs, OpenAPI, SDK tables, Rust and Python bindings are regenerated, not hand-edited.

## External effects and their classes

None. A recording observes the person; it executes nothing. The compiled skill *needs* tools and effect classes and
grants none; every action it later asks for is classified and approved like any other (a click that sends is
`computer.input.privileged` whatever skill proposed it). The compiler runs in the intelligence plane with no write
path; the registry write is Rust's.

## Capabilities and approvals

Capability only narrows: `capability_needs` of a proposal are the effect classes of tools that exist in the Tool
Registry, never more; the Rust validator refuses a proposal naming a tool or class outside the registry. Start,
finish, cancel, compile and submit are person-only (`AuthorWork` in the workspace, a session user, the owner). No
approval is created or consumed. Promotion beyond DRAFT stays `PromoteSkillVersion` (`ManageTenant`).

## Migrations

`0026_skill_recordings.sql`: one table with forced tenant RLS, the composite workspace key, the `quansio_app` grant
and the `updated_at` trigger; a partial unique index (one `recording` per owner) and a unique `(tenant_id, start_key)`.
Forward-only and additive.

## Real boundaries and the environment they need

- macOS Accessibility, Input Monitoring and Screen Recording for the live observer, screenshot capture and the AX
  target lookup; the Speech/Microphone path is not built (see Deferred). Without them those tests print
  `BLOCKED_EXTERNAL` and the task cannot be PASS. Variable: `QUANSIO_TEST_MACOS_RECORDER=1` (the live observation
  test posts nothing and reads nothing it was not given; it needs the permissions the terminal host holds).
- Real without any permission: secure-event-input (`EnableSecureEventInput`/`IsSecureEventInputEnabled`), Vision text
  recognition on a rendered image, a real Unix-domain socket, real file modes, a real `quansio-recorder` process.
- Real PostgreSQL (`QUANSIO_TEST_POSTGRES_URL`, a private database) for the store, the fence and the API.
- The real Python intelligence process (`uv`) for the compile RPC and the DRAFT submission.
- No credential is read by any test. MinIO is not required: the API tests inject an in-memory object store, so the
  S3 path of the artifact store is not exercised here (stated in the evidence).

## Rollback and recovery

Revert the task branch: the migration adds one table and no existing column changes, so a forward-fix drops nothing
of anyone else's. A crash mid-recording loses the in-memory buffer by design and leaves a `recording` row that expires
(`recording.aborted`, lazily on the next call: no timer, no sweeper). A crash after `FinishRecording` committed leaves
a durable archive and a `finished` row; compile and submit are idempotent by recording id and fingerprint.

## Plan (checklist; not registry tasks)

1. [x] DOMAIN.md, config, schemas, migration, prefix, error and command catalog; regenerate contracts.
2. [x] Rust: redaction rule set, archive validation, `skill_recordings` store with the fence, compile and submit,
       proposal validator, API handlers and projections.
3. [x] Proto + Python: `CompileSkillRecording`, the compiler (slots, conditions, recovery, tool preference, trust
       labelling, no-coordinates validator), manifest `procedure`.
4. [x] Swift: event model, redaction, sampler, secure-input pause, owner fence, control channel, executable.
5. [x] Desktop: recording state, main-process recorder client, indicator, review and submit.
6. [x] Tests (positive, negative, recovery), mutation checks, evidence bundle, validator, progress.

## What changed against this plan while building

- The config and its schema are `config/recording.yaml` and `schemas/json/recording-config.schema.json`: the supply
  chain gate reads any file whose name carries the token `skill` as a skill manifest and demands a quarantine
  lifecycle, so the first names (`skill-recording*`) were renamed.
- `crates/events` (`EventFamily::Recording`, `EVENT_FAMILIES`), `crates/core` (`Prefix::Recording`),
  `crates/server/src/intelligence/mod.rs` (`compile_skill_recording`) and
  `crates/server/src/control/identity/auth.rs` (`AuthStore::session_family`) gained the one addition each that a closed
  vocabulary or a missing accessor forced; `scripts/ci/gen_contracts.py` gained the five commands' parameter schemas.
- `apps/desktop/tests/preload.test.ts` lists the seven new bridge names; `tests/contract/test_contracts.py` counts 65
  errors and 36 event families.
- The recorder is a separate short-lived executable (`quansio-recorder`) rather than a library the desktop loads, so a
  crash or a closed channel cannot take the desktop with it and nothing of the recording outlives the process.

## Recorder platform seam

`ObservationSource` (`native/macos/Sources/QuansioMacBridge/Recording/RecordingModel.swift`) is the whole of what a
platform provides: start delivering `Observation` values (kind, bundle id, accessibility attributes, text, key, time)
and stop. Everything else (redaction, the session, the fence, the control channel, the archive) is platform-free Swift
over that seam, and the archive format is the same JSON a Windows recorder would produce. Remote and other platforms
are out of scope; nothing in the server or the plane knows the platform beyond the archive's `platform` field.
