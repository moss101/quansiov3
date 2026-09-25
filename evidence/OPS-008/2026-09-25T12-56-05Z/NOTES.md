# OPS-008 evidence notes: operational runbooks and kill switches

The switches are durable. Each engagement is an `ops_switches` row (`ksw_`), kept as history,
with at most one active row per target. Engaging and releasing are `runtime.control` actions for
tenant owners and administrators: each commits with its `ops.kill_switch_*` event and an
AuditEntry carrying the reason. Enforcement belongs to the owners of what a switch stops, and
they read `OpsGate`.

| Acceptance / test | Test (`crates/server/tests/ops_kill_switches.rs` unless noted) |
|---|---|
| **Kill-switch E2E**; "effect freeze blocks new consequential actions without corrupting reads or active evidence" | `effect_freeze_stops_new_consequential_actions_without_touching_reads_or_evidence`, through real turns (capability, policy, Effect Ledger). A settled tier 2 effect and a parked tier 3 write exist before the freeze. `FreezeEffects` has these effects: (a) a person's approval of the parked write is stopped before reservation (the effect stays `PROPOSED` and the receipt is not consumed); (b) a tier 0 read dispatches; (c) a tier 2 navigation is refused `EFFECTS_FROZEN` and never reaches the host; (d) the settled effect row is byte-identical; (e) `GET /v1/effects` still reads it. A second freeze is 409; `/v1/ops/switches` shows it active with its reason. `UnfreezeEffects` restores tier 2 dispatch, and a second release is 404. |
| "Kill switches are audited and recoverable" | Every test engages and releases through the public command surface. `ops.<switch>.engage` / `.release` audit rows name `runtime.control (tier 3): <reason>`, and the `ops.kill_switch_engaged` / `_released` events are asserted. |
| Emergency deny | `emergency_deny_refuses_matching_classes_before_policy_and_scoped_freezes_stay_scoped`. `network.*` refuses navigation (`POLICY_DENIED`) while reads run, and release restores it. A freeze naming an unknown workspace is 404. An unknown switch, a bad target and a missing reason are each 400. |
| **Worker quarantine test** | `worker_quarantine_fences_the_target_and_release_restores_dispatch`. The worker's lease is revoked and the target generation moves (the old worker is fenced). Every call of a run on the target is refused `TARGET_UNAVAILABLE` and nothing reaches the host. Release restores dispatch, and a fresh lease is granted. |
| Target drain | `drain_and_undrain_move_the_target_lifecycle_and_are_audited`. The target goes `DRAINING` and no lease is granted; a double drain is 409; release returns it to `READY` (the new `DRAINING → READY` edge) and a lease is granted; an unknown target is 404. |
| Connector revoke | `connector_revoke_goes_through_the_connector_owner`: `RevokeConnector` revokes the instance (credential handle cleared) through the connector owner, evented and released. |
| Stream shed | `stream_shed_disconnects_live_sessions_with_backpressure`. Two live WebSocket sessions each get `STREAM_BACKPRESSURE` and are closed. A reconnecting client is served. |
| **Provider outage drill** | `provider_outage_drill_fails_over_refuses_and_restores` runs against the **real Python plane** and the dev stub provider, with a two-provider catalog. With the primary disabled, calls fail over to the backup (`model_routes.provider`). With both disabled, the call is refused `PROVIDER_UNAVAILABLE` and no model call is recorded. After enabling, calls return to the primary. In the plane: `python/tests/intelligence/test_model_provider_disable.py` shows the only request on the wire goes to the enabled provider's model, and nothing is sent when every provider is disabled. |
| Runbooks | `the_runbook_is_generated_from_the_switch_catalog`: `docs/runbooks/kill-switches.md` equals `runbook_markdown()`. It is regenerated with `QUANSIO_WRITE_RUNBOOKS=1`. |

Real boundary: none (`real_boundary=false`). The provider drill's plane and stub provider are the
deterministic conformance boundary, not live provider evidence.
