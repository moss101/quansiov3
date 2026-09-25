# Kill switches (OPS-008)

<!-- GENERATED from crates/server/src/control/ops/mod.rs by runbook_markdown(); regenerate with QUANSIO_WRITE_RUNBOOKS=1 cargo test -p quansio-server --test ops_kill_switches. -->

Not authority: DOSSIER.md §21.5 and DOMAIN.md §7.6 are. Every switch is a `runtime.control` action (tier 3, tenant owners and administrators), audited on engage and release, emitted as `ops.kill_switch_engaged` / `ops.kill_switch_released`, and reversible: releasing restores service and nothing is deleted. `GET /v1/ops/switches` shows what is engaged, by whom and why.

Generic commands for every switch: `EngageKillSwitch {switch, target, reason}` and `ReleaseKillSwitch {switch, target, reason?}`.

## 1. `provider.disable`

- **Target:** a provider name from config/models.yaml (`anthropic`, `openai`, ...)
- **Engage:** `DisableProvider {provider, reason}`
- **While engaged:** every model call names the provider as disabled; the gateway fails over to an enabled fallback route and refuses with PROVIDER_UNAVAILABLE when none is left
- **Untouched:** calls already streaming finish; routes to other providers are unaffected
- **Release:** `EnableProvider {provider}`
- **Recovery:** release; the next model call may route to the provider again

## 2. `connector.revoke`

- **Target:** a connector instance id (`cnx_...`)
- **Engage:** `RevokeConnector {instance_id, reason}`
- **While engaged:** the instance is revoked and its credential handle cleared; no dispatch can use it
- **Untouched:** evidence and artifacts produced through the connector
- **Release:** `ReleaseKillSwitch {switch: connector.revoke, target}`
- **Recovery:** reconnect with BeginConnectorAuth / CompleteConnectorAuth, then release to close the incident

## 3. `worker.quarantine`

- **Target:** an execution target id (`tgt_...`), the worker that serves it
- **Engage:** `QuarantineWorker {target_id, reason}`
- **While engaged:** the target's lease is revoked (its generation moves, fencing the worker); every tool call of a run on it is refused TARGET_UNAVAILABLE
- **Untouched:** other targets and their workers; effects already in the ledger, which reconcile rather than re-dispatch
- **Release:** `ReleaseKillSwitch {switch: worker.quarantine, target}`
- **Recovery:** release; runs on the target dispatch again and acquire a fresh lease

## 4. `target.drain`

- **Target:** an execution target id (`tgt_...`) that is READY or BUSY
- **Engage:** `DrainTarget {target_id, reason}`
- **While engaged:** the target moves to DRAINING: no new lease is granted, in-flight work finishes
- **Untouched:** the running lease and its work
- **Release:** `ReleaseKillSwitch {switch: target.drain, target}`
- **Recovery:** release returns a DRAINING target to READY; a target that already stopped is restored or replaced by its lifecycle

## 5. `effect.freeze`

- **Target:** `*` for the tenant, or a workspace id (`ws_...`)
- **Engage:** `FreezeEffects {workspace_id?, reason}`
- **While engaged:** new tier >= 2 effects are refused EFFECTS_FROZEN before any effect is reserved or approval requested; an approved effect is refused before reservation
- **Untouched:** tier 0/1 calls, every read, the evidence store, and effects already in the ledger
- **Release:** `UnfreezeEffects {workspace_id?}`
- **Recovery:** release; runs propose again and nothing needs repair

## 6. `policy.emergency_deny`

- **Target:** an effect class (`message.send`), a family prefix (`scm.*`), or `*`
- **Engage:** `EngageKillSwitch {switch: policy.emergency_deny, target, reason}`
- **While engaged:** matching tool calls are refused POLICY_DENIED before policy is evaluated
- **Untouched:** calls of other classes; stored policies and user rules
- **Release:** `ReleaseKillSwitch {switch: policy.emergency_deny, target}`
- **Recovery:** release; ordinary policy decides again

## 7. `stream.shed`

- **Target:** `*`
- **Engage:** `EngageKillSwitch {switch: stream.shed, target: *, reason}`
- **While engaged:** every live /v1/stream subscriber of the tenant is disconnected with STREAM_BACKPRESSURE; while engaged a lagging subscriber is disconnected instead of missing frames
- **Untouched:** every durable event: clients reconnect with their cursor and lose nothing
- **Release:** `ReleaseKillSwitch {switch: stream.shed, target: *}`
- **Recovery:** release; nothing else is needed

## Incident drill

1. Engage the narrowest switch that stops the harm (a workspace freeze before a tenant freeze; one provider before an emergency deny), with a reason that names the incident.
2. Confirm it in `GET /v1/ops/switches` and in `GET /v1/audit`.
3. Fix the cause. Effects left `OUTCOME_UNKNOWN` reconcile; they are never re-dispatched.
4. Release with a reason, and confirm the release event and audit entry.
