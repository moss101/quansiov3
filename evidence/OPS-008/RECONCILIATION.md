# OPS-008 reconciliation: operational runbooks and kill switches

**Canonical owner** (the task's `paths`):
- `crates/server/src/control/ops/`: switch state, engage/release, the per-dispatch gate, the
  runbook generator;
- `docs/runbooks/`: generated runbooks.

The enforcement points belong to the owners of what they stop, and each one reads the ops owner's
gate rather than the table:
- runtime dispatch;
- the model source;
- machine control (target lifecycle);
- connectors;
- the live stream hub.

**Existing code (2026-09-25).** Classification: PARTIAL, a model with nothing using it.
- `control/ops/mod.rs` is an in-memory `KillSwitchBoard` with the seven DOSSIER §21.5 switch
  names. Nothing outside its test uses it: it has no table, no command and no enforcement.
- The §14 commands `FreezeEffects`, `UnfreezeEffects`, `DisableProvider`, `EnableProvider`,
  `QuarantineWorker`, `DrainTarget` and `RevokeConnector` have no handler.
- `EFFECTS_FROZEN` is in the error catalog, and nothing raises it.
- Hook points:
  - `ToolDispatchService::dispatch`: after policy, before `insert_tool_call` / `reserve` /
    `park_for_approval`.
  - `ToolDispatchService::resume`: before `reserve_authorized`, for an approved effect.
  - `GatewayModelSource`: builds the gateway `ModelCallRequest`.
  - `MachineControl::{transition, revoke_lease}`: `DRAINING` exists, one-way to `STOPPED`.
  - `ConnectorStore::transition(Revoked)`.
  - `LiveHub` / `stream.rs` `tail_events`: currently skips lagged frames silently.
  - Audit `append_in`; the `ops.*` event family (registered, never emitted); RBAC.

**Design:**
- **Durable switches.** `ops_switches` stores one row per engagement, keeping history, with at
  most one active row per `(tenant, switch, target)`. Rows are `ksw_` ids (new DOMAIN §1.1 prefix)
  under tenant RLS.
- **Engage and release.**
  - Each is a `runtime.control` action (DOMAIN §7.1: tier 3, administrators only, audited).
  - Each commits with its `ops.kill_switch_engaged` / `ops.kill_switch_released` event and an
    AuditEntry carrying the reason.
  - Releasing restores service. Nothing is deleted.
- **The switches:**
  - `effect.freeze` (`*` or a `ws_` id): dispatch refuses new tier ≥ 2 effects with
    `EFFECTS_FROZEN`, before any tool call, ledger row or approval request is written. An
    approved effect resuming under a freeze is refused the same way before reservation. Tier
    0/1 calls, reads, the evidence store and effects already in the ledger are untouched.
  - `policy.emergency_deny` (an effect class, a `family.*` prefix, or `*`): dispatch refuses
    matching calls with `POLICY_DENIED` before policy evaluation.
  - `provider.disable` (a provider name): each model call names the disabled providers
    (`ModelCallRequest.disabled_providers`, additive). The gateway routes around them to an
    enabled fallback and refuses with `PROVIDER_UNAVAILABLE` when none is left.
  - `worker.quarantine` (a `tgt_` execution target):
    - Engaging revokes the target's current lease, which bumps its generation and fences the
      worker.
    - Dispatch refuses every tool call of a run on that target with `TARGET_UNAVAILABLE`.
    - Releasing restores dispatch.
  - `target.drain` (a `tgt_` target):
    - Engaging moves the target to `DRAINING`: no new lease, and in-flight work finishes.
    - Releasing returns it to `READY`. That needs a new `DRAINING → READY` edge, recorded in
      DOMAIN §8.2.
  - `connector.revoke` (a `cnx_` instance): the connector owner revokes the instance, clearing
    its credential handle. It is terminal by design (credential compromise). Recovery is
    reconnecting (`BeginConnectorAuth`), and releasing closes the incident record.
  - `stream.shed` (`*`): engaging disconnects every live `/v1/stream` subscriber of the tenant
    with `STREAM_BACKPRESSURE`. They reconnect with their cursor and lose nothing durable. While
    engaged, a lagging subscriber is disconnected instead of silently missing frames.
- **Commands** (DOMAIN §14 Admin/ops, additive):
  - the existing named ones, mapped onto the switches;
  - `EngageKillSwitch {switch, target, reason}` and `ReleaseKillSwitch {switch, target,
    reason?}` for every switch;
  - `RevokeConnector` (Connectors family).
  All need manage_tenant; a service principal may use them only with the Admin/ops family.
- **Read projection:** `GET /v1/ops/switches`, the active switches and recent history (tenant
  administrators).
- **Runbooks:** `docs/runbooks/kill-switches.md` is generated from the switch catalog in
  `control/ops` (`runbook_markdown()`). A test fails when the committed file drifts, and
  `QUANSIO_WRITE_RUNBOOKS=1` rewrites it.

**Persistent state:** migration `0021_ops_switches.sql` (additive).

**Contracts (additive):**
- DOMAIN §1.1: `KillSwitch | ksw_`.
- DOMAIN §7: the switch semantics.
- DOMAIN §8.2: `DRAINING → READY`.
- DOMAIN §11.1: `disabled_providers`.
- DOMAIN §14: the two commands and `/ops/switches`.
- Proto: `ENTITY_PREFIX_KILL_SWITCH = 53`, and `ModelCallRequest.disabled_providers = 16`.

**Effects:** `runtime.control` only (the switch itself). **Real boundaries:** none
(`real_boundary=false`).

**Rollback:** release every switch. The table and fields are additive.

## Plan

- [ ] migration 0021 + README; DOMAIN + proto + regenerate
- [ ] `control/ops`: catalog, store (engage/release/list), gate, runbook generator
- [ ] enforcement: dispatch + resume (freeze, emergency deny, quarantine); model source + Python
      gateway (provider disable); machine drain/undrain + lease revoke; connector revoke;
      live-hub shed
- [ ] API: named + generic commands, `RevokeConnector`, `/v1/ops/switches`
- [ ] tests:
  - kill-switch E2E through real turns: freeze blocks tier ≥ 2 without touching reads,
    evidence or existing effects, including an approved effect resuming;
  - emergency deny;
  - audit/event/release;
  - worker quarantine;
  - drain/undrain;
  - connector revoke;
  - stream shed;
  - provider outage drill against the real Python plane (fail over, then no provider left);
  - runbook drift;
  - mutation checks.
- [ ] evidence, CI
