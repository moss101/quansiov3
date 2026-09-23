# APP-015 — public event stream and outbound webhooks

Ownership and design decisions: `evidence/APP-015/RECONCILIATION.md`. The previous
`notify/webhooks.rs` was a placeholder (a predicate, a `contains` and a key-name redactor;
see `docs/review/2026-09-18-m5-placeholder-audit.md`) and is replaced.

## What was built

- **Contracts (DOMAIN, additive):** §13.5 fully specified (grant semantics, delivery ledger,
  retry/backoff/dead-letter, attempt-time egress, request/signature, redaction, projections);
  §9.2 `webhook.*` family; §9.3 SSE transport; §14 `UpdateWebhookSubscription`,
  `RedeliverWebhook`, `/webhooks`, `/webhooks/{id}/deliveries`, `GET /v1/stream/events`;
  §1 `whd_`; §7.3 `allowed_address_classes` (see "Found on the way"). Contracts regenerated
  (`gen_contracts.py --all`): catalog (new `webhooks.yaml`), proto `ENTITY_PREFIX_WEBHOOK_DELIVERY`,
  OpenAPI, TS types, SDK tables.
- **Migration `0017_webhook_deliveries.sql`:** `webhook_subscriptions.created_by`,
  `start_sequence`; `webhook_deliveries` (RLS, unique (subscription, event)).
- **`crates/server/src/notify/webhooks/`:** `store` (create/update/rotate/delete/redeliver with
  a `webhook.*` event and an AuditEntry in the same transaction; the signing secret generated,
  sealed as a SecretHandle and returned once), `fanout` (cursor consumer; enqueues in the
  cursor-advancing transaction; tenant/workspace/start-sequence scope; `webhook` family,
  notifications and protected threads withheld), `deliverer` (lease, policy read at the attempt,
  resolution, `EgressBroker::decide` with origin `webhook`, secret materialized at the boundary
  with `MaterializationScope::Webhook`, request pinned to the vetted addresses with redirects
  off, backoff 30 s·2^(n−1) ≤ 1 h, dead-letter after 8 → `failing`, recovery on success, lease
  fencing), `signature` (`t=…,v1=…` HMAC-SHA256), `redact`.
- **API:** four commands (admin RBAC), two projections, the command log never keeps a one-time
  secret (`stored_result`); `ApiState::with_key_provider`; composition reads
  `QUANSIO_SECRETS_KEY_FILE` (local master key, DOSSIER §15) and runs the webhook pass on its
  own loop beside the routines loop.
- **SSE:** `GET /v1/stream/events` beside the WebSocket, same channels, frames, gate; `id:` is
  the cursor and `Last-Event-ID` resumes.
- **OpenAPI:** `/v1/stream` (WebSocket, subscribe + frame schemas), `/v1/stream/events` (SSE),
  top-level `webhooks.runtimeEvent` (headers, redacted `RuntimeEvent` body from
  `schemas/json/runtime-event.schema.json`), `x-quansio-webhook-signature`, concrete params for
  the four commands (`x-quansio-params-defined: true`).
- **SDKs:** generated `WEBHOOK_SIGNATURE` (TS) / `WEBHOOK_*` (Python) constants from DOMAIN
  §13.5; `verifyWebhookSignature` (Web Crypto) and `verify_webhook_signature` (stdlib), both
  tested against the vector the server's own unit test asserts (the value was computed
  independently with Python's `hmac`).

## Acceptance → tests (`crates/server/tests/webhooks.rs`, real PostgreSQL + a real HTTP receiver)

- *At-least-once with stable event ids; no delivery outside tenant/workspace.*
  `delivery_is_signed_at_least_once_and_retried_until_dead_lettered`: one delivery per event
  however the log is consumed (a second scan, a lost cursor); a 500 is retried after backoff
  with the same `quansio-event-id`, the same body and a verifying signature; 8 failures
  dead-letter and mark the subscription `failing` (one `webhook.delivery_dead_lettered`);
  `RedeliverWebhook` re-queues and recovery sets it `active`; a lease held by a dead instance is
  re-attempted once it lapses; rotation invalidates the old secret; delete revokes the handle.
  `deliveries_stay_in_their_tenant_and_workspace`: workspace vs tenant-wide vs another tenant's
  subscription each receive exactly their events; protected-thread and notification events are
  never delivered; tenant B can neither list nor change A's subscriptions; viewers cannot manage.
- *Destinations obey egress policy; kill switch.* `destinations_obey_egress_policy_and_the_kill_switches`:
  no network rule / a `deny` rule → `POLICY_DENIED`; loopback under a public-only policy,
  link-local metadata, `metadata.google.internal`, `localhost` → `EGRESS_DENIED`; credentials in
  the URL → 400; a name that resolves to loopback → `egress_denied:address_not_allowed`, an
  unresolvable one → `egress_denied:unresolved`, nothing sent; pausing holds deliveries (sent on
  resume); `SetPolicy` deny stops a queued retry and a new event; no key provider → nothing sent.
  `a_deployment_without_a_key_provider_refuses_to_create_a_webhook`.
- *Secrets and protected classes never in payloads.* `payloads_never_carry_secrets_or_protected_data`:
  canary, `sk-`/`ghp_` tokens, bearer header, password/refresh-token values, an email, and a
  `pii`/`financial` object's title and card number never reach the receiver (headers or body);
  the signing secret is found nowhere but sealed (commands, events, audit, handles, subscriptions).
- SSE: `the_event_stream_is_also_served_as_server_sent_events`.
- Unit: signature (incl. the cross-language vector), redaction, filters, backoff, SSE query
  parsing, one-time-secret stripping, `allowed_address_classes` parsing.
- SDKs: `sdk/typescript/tests/webhooks.test.ts`, `tests/sdk/test_python_sdk_webhooks.py`.

## Negative checks (the tests fail when the statement is false)

See `mutation-checks.log`: each guard was removed in a scratch edit (not committed) and the
named test failed; the fix restored it.

## Found on the way

- EXEC-008's broker reads `conditions.allowed_address_classes`, which neither DOMAIN §7.3 nor
  RUN-006's `RuleConditions` (`deny_unknown_fields`) had: a policy with it failed to parse and
  `SetPolicy` refused it. Added additively (validated against the broker's classes, omitted when
  empty so existing digests are unchanged).
- `commands.id` is globally unique although idempotency is per tenant — two tenants using one
  command id collide with a 500. Out of scope; raised as a separate task.
- Not in scope and not done: a desktop/web admin screen for webhooks; the dev scripts do not
  provision `QUANSIO_SECRETS_KEY_FILE` (the server says so at startup and refuses to create a
  webhook without it). OPS-008's durable effect freeze will plug into the attempt gate.

## Full CI

`bash scripts/ci/ci.sh` on this branch after merging main: 11/11 gates PASS
(`ci-summary.json`). Earlier runs on the way: the first failed only on
`apps/desktop/tests/collaboration.e2e.test.ts` (an intermittent `ACTIVE -> ACTIVE` race in
APP-009's message routing, fixed and recorded as an APP-009 regression in
`evidence/APP-009/2026-09-23T10-16-25Z/`, merged to main first); the second failed only the
two SDK compatibility tests because `target/debug/quansio-server` still embedded main's
command catalog after the branch round-trip — a fresh `cargo build -p quansio-server` and the
third run passed.
