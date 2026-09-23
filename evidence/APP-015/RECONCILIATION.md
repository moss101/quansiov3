# APP-015 reconciliation — public event stream and outbound webhooks

**Canonical owner.** `crates/server/src/api/webhooks.rs` (commands, projections) and
`crates/server/src/notify/webhooks/` (subscriptions, fan-out, delivery ledger, signing,
redaction, delivery through the egress broker), matching the task's `paths`. The SSE form of
the public stream lives beside the WebSocket in `crates/server/src/api/stream.rs` (CORE-009's
owner) and reuses its subscription, cursor and ThreadGate.

**Existing code.** `notify/webhooks.rs` (81 lines) is a `may_deliver` predicate, an
`already_delivered` `contains`, and a key-name `redact`; `api/webhooks.rs` re-exports them;
`tests/webhooks.rs` tests those predicates. Classification: PARTIAL placeholder (see
`docs/review/2026-09-18-m5-placeholder-audit.md`), replaced. `webhook_subscriptions` exists in
0001 (RLS + updated_at trigger already applied there). Reused, not duplicated:
`quansio_machine::egress::{EgressBroker, EgressRequest, EgressGrant, NetworkPolicy,
Destination, EgressOrigin::Webhook}` (EXEC-008 — "webhook and connector egress use the same
broker"), `quansio_machine::secrets::{SecretBroker, LocalMasterKeyProvider}` (EXEC-007) for
the signing secret, `observability::redact::redact_text` (OPS-003 module, previously
unreferenced) for value scrubbing, `policy::guards::DataClass::PROTECTED` for protected
classes, `quansio_events::{EventStore, advance_cursor_in_tx}` for the fan-out cursor (as the
APP-010 routine triggers do), `control::collaboration::ThreadGate` for protected threads,
`background_tenants()` + the composition `Background` loop.

**Persistent state.** Migration `0017_webhook_deliveries.sql`: `webhook_subscriptions` gains
`created_by`, `start_sequence`; new `webhook_deliveries` (RLS, unique (subscription, event)).
Secret handles (`secret_handles`, provider `webhook`) hold signing secrets, sealed by the
server's `KeyProvider` (`QUANSIO_SECRETS_KEY_FILE`, DOSSIER §15 "local master key file" for
dev/self-host). `event_cursors` row for the fan-out consumer.

**Contracts/events.** DOMAIN §13.5 extended (subscription fields, grant semantics, delivery
ledger, signature, redaction, kill switch); §9.2 adds the `webhook.*` family; §14 adds
`UpdateWebhookSubscription`, `RedeliverWebhook`, read projections `/webhooks`,
`/webhooks/{id}/deliveries` and `GET /v1/stream/events` (SSE); §1 id table adds `whd_`.
OpenAPI documents the stream (WS + SSE) and the webhook payload/signature; SDKs get
generated signature constants plus verification helpers.

**External effects.** Outbound HTTPS POST to integrator URLs: `network.egress.new_destination`
(tier 2). The subscription is the destination's grant (admin-created, audited); every attempt
is decided by the egress broker (origin `webhook`) against the *current* tenant/workspace
network policy and the addresses the host resolves to now, and the request is pinned to the
vetted addresses with redirects off. Not an EffectRecord per delivery: the ledger's
reserve/settle model is run-scoped (capability projection, workspace, OUTCOME_UNKNOWN blocks
the key), and re-sending the same event id is the at-least-once contract, not a blind retry
of an unknown effect — consumers dedupe on `event_id`.

**Capabilities/approvals.** Webhook management is admin-only (RBAC `identity.change`-class
admin action), audited. No agent/model path can create or change a subscription.

**Kill switch.** Per destination: `UpdateWebhookSubscription {status: paused}` holds its
deliveries. Tenant-wide: the canonical Policy's `network.egress.new_destination` rule set to
`deny` (SetPolicy — DOSSIER §21.5 "policy emergency deny") stops every attempt, including
queued retries, because policy is read at the attempt. OPS-008's durable effect freeze will
plug into the same attempt gate.

**Real boundaries.** None required (`real_boundary: false`); tests use a local HTTP receiver
against real PostgreSQL.

**Rollback/recovery.** A delivery leased by a crashed instance is re-attempted after its
lease lapses (at-least-once). The migration is additive.

## Plan

- [x] DOMAIN §1 (`whd_`), §9.2 (`webhook.*`), §13.5, §14; regenerate contracts; `Prefix::WebhookDelivery`.
- [x] Migration 0017 (subscription columns, `webhook_deliveries`).
- [x] `MaterializationScope::Webhook` (additive) for the signing-secret boundary.
- [x] `notify/webhooks/`: types + event filter + redaction + signature; store (create/update/
      delete/rotate/redeliver/list); fan-out consumer (cursor, tenant/workspace scope,
      start_sequence, protected threads, `webhook` family excluded); deliverer (lease, policy +
      DNS + broker, pinned request, backoff, dead-letter, `failing`).
- [x] `api/webhooks.rs`: four commands (admin RBAC, audit, idempotent), `/v1/webhooks`,
      `/v1/webhooks/{id}/deliveries`; `KeyProvider` on `ApiState`; composition loop.
- [x] SSE `GET /v1/stream/events` beside the WebSocket.
- [x] OpenAPI: stream paths, `webhooks` payload + signature; SDK constants + verifiers (TS, Python).
- [x] Tests: delivery/retry (+ dead-letter, redelivery, crash lease), tenant scope, redaction scan,
      egress deny (+ kill switches), SSE, SDK verifiers against a Rust-produced vector.

## Found on the way

- EXEC-008's broker reads `conditions.allowed_address_classes` from a network rule, but the
  field was in neither DOMAIN §7.3 nor RUN-006's `RuleConditions` (`deny_unknown_fields`): a
  policy carrying it failed to parse (every agent decision under it denied) and `SetPolicy`
  refused it. Added additively to both (`policy/rules.rs`, validated against the broker's
  address classes, omitted from serialization when empty so existing policy digests are
  unchanged), with a unit test.
- `schemas/json/runtime-event.schema.json` enumerates the event families in its `type`
  pattern; `webhook` added with the family.
- `commands.id` is the client command id and is globally unique (the idempotency conflict
  target is `(tenant_id, command_id)`): two tenants reusing one command id collide with a
  500. Raised as a separate task; fixed on main as an APP-001 regression (5c5334e).
