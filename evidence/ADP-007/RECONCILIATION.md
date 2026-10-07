# ADP-007 reconciliation (2026-10-07, agent:adp-007)

Automation triggers: inbound hooks, dry run, optimistic concurrency, idempotent creates, per-plan limits, IANA time zones and
journaled workflow scripts. Dependencies APP-010, RUN-011 and INT-012 are PASS; `real_boundary` is false.

## Canonical owners (match the task's `paths`)

| Concern | Owner | Notes |
|---|---|---|
| Routine definitions, revisions, hooks, the delivery ledger, filters, scripts (IR, validation, templates), the dry run | `crates/server/src/control/routines/` | APP-010's owner; `routine_fires` stays the one firing ledger |
| Time zones, the script executor | `crates/server/src/scheduler/` | CORE-008's scheduler; `scheduler/timezone.rs` replaces the fixed-offset parser, `scheduler/workflow.rs` is the executor |
| Authenticating an inbound request, notifying a paused script | `crates/server/src/notify/` | `notify/webhooks/inbound.rs` reuses the outbound signature scheme and the secret broker; the notifier gains `routine.script_paused` |
| Wiring only | `api/hooks.rs`, `api/routines.rs`, `api/commands.rs`, `api/trust.rs`, `composition.rs`, `intelligence/model_source.rs` | no new authority; handlers call the owners |

## Existing code and its coverage

* `control/routines` (store, ledger, event triggers), `scheduler::RoutineScheduler`, `FireExecutor`, `InteractiveDispatch`:
  PARTIAL, kept and extended. A fire is already one `routine_fires` row under a unique key, claimed before execution and
  leased while it runs; an inbound delivery is one more source of a claimed fire.
* Fixed-offset time zones (`parse_timezone`): replaced by `ScheduleZone` over the IANA database; unknown names still fail
  closed, now with a typed code instead of `UnsupportedTimezone`.
* Outbound webhooks (APP-015): the `quansio-signature` scheme (`sign`/`verify`), `generate_secret`, the SecretHandle broker and
  `MaterializationScope` are reused unchanged; no secret handling is re-implemented.
* INT-012 content trust: the context plane already renders `untrusted_external` segments inside the typed boundary and the
  runtime already derives `derived_from_trust` from the turn's recorded projection. `GatewayModelSource` built one segment per
  message and labelled by author; it now builds one segment per run of blocks of one trust.
* RUN-003 `Planner`, GraphTransaction, `AgentStore` delegation, `RuntimeEngine`, the Effect Ledger and the approval path
  (`ApprovalRuntime`, `PolicyStore`): reused for scripts; nothing is added beside them.
* No connector event source exists (nothing emits `connector.webhook_received`); a connector-event trigger is therefore only an
  `event` trigger over `connector.*`, accepted and inert. Recorded as a deferral, not built.

## Persistent state touched (migration 0028, additive, in the routine owner)

* `routines`: `revision`, `create_request_id`, `create_digest`, `hook_endpoint_id`, `hook_secret_handle_id` (+ two unique indexes).
* `routine_fires`: `payload JSONB`; source `hook` admitted.
* `routine_hook_deliveries` (RLS enabled and forced, `quansio_app` grant): the replay ledger, unique on delivery key and on
  signature digest.
* `routine_hook_lookup(endpoint)` (`SECURITY DEFINER`, ids only): an inbound request carries no tenant.
* Workflow journal: **no table and no column.** The journal is Evidence on the script run (`kind: log`, captured by
  `runtime/workflow`), the call records are Run and WorkNode rows, the gates are EffectRecords and ApprovalRequests. Reconciled
  against the brief's "unless the owners cannot hold it": they can, because a result is bounded (64 KiB) and Evidence rows are
  insert-only, tenant-scoped and already the layer recovery reads (DOSSIER section 8, layers 5 to 8).

## Contracts and events (DOMAIN.md changed in the same commits)

13.8 (new), glossary (6 terms), 3.2 (`untrusted_data` block), 9.2 (`routine.hook_rotated`, `script_*`), 12 rule 9, 13.1 (trigger
kinds, revision, fire key), 14 (`DryRunRoutine`, `RotateRoutineHook`), 15 (family Automation, 4 codes). Generated: commands,
errors, glossary catalogs, OpenAPI (+ `POST /v1/hooks/{endpoint_id}`), TypeScript and both SDK operation tables.

## External effects and classes

None new. A script gate is an EffectRecord of the existing class `record.create` on `{kind: work, selector: "workflow:<run>"}`
(the class `work.propose_plan` and `work.delegate` already settle as); a worker's own tool calls take the ordinary path. No
effect class, store or service is added (D-019).

## Capabilities and approvals

A hook fire's run is seeded exactly as an interactive run's. A payload names no capability. Fan-out above
`script.approval_threshold_calls` and an extension of the call budget need an ApprovalRequest through the existing approval path;
the run that carries it makes no model turn, so `approve_effect` / `deny_effect` skip the resumed turn for a gate effect.

## Dependency (DOSSIER section 18)

`chrono-tz` 0.10.4 (MIT OR Apache-2.0; pulls `phf` 0.12 and `siphasher`, same licences): the IANA zone database compiled in, so
a schedule means the same instants on every host. It is the crate `chrono` itself points to for zones, needs no system tzdata and
no build script network access (prebuilt tables). `deny.toml` licence allow-list covers it.

## Real boundaries and credentials

`real_boundary: false`. Tests use the real PostgreSQL (private database), the real scheduler/timer path, real HTTP listeners and,
for the untrusted-label proof, the real Python intelligence process with the dev stub provider. Variables:
`QUANSIO_TEST_POSTGRES_URL`; for the plane test also `uv` and `QUANSIO_TEST_STUB_PROVIDER_URL`. No live model provider is used.

## Rollback and recovery impact

Migration 0028 is additive; a pre-0028 row has revision 1, no hook and no payload. A hook can be removed by deleting its routine
(the secret is revoked). Recovery: a claimed fire is leased and resumed; a script run resumes from graph plus journal.

## Open decisions touched

None of DOSSIER section 23. The brief's reference extractions were not present on disk; the design came from the brief itself.
