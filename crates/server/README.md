# quansio-server

**Canonical owner:** `crates/server` — quansio-server: authoritative API, control, runtime, policy, effects, scheduler and artifact composition.

Owns only the responsibilities named in DOSSIER.md §5. It must not become a
second runtime, store, policy engine or effect path: all state transitions go
through the canonical Quansio primitives described in `DOSSIER.md`.

The client for the Python intelligence gateway (`src/intelligence`) is the only
way the runtime reaches the intelligence plane: a separate process, over gRPC.
With `QUANSIO_INTELLIGENCE_ENDPOINT` set, turns are answered through it
(`GatewayModelSource`); unset, by the conformance stub, and startup says so.
With `QUANSIO_NATS_URL` set, `OutboxRelay` publishes the outbox to NATS JetStream.
`QUANSIO_SECRETS_KMS_KEY_ID` seals secret handles under AWS KMS (`src/kms.rs`).

Build: `cargo test -p quansio-server`

Observability (`src/observability`, OPS-003): W3C trace context across processes, redacted
JSON logs on stderr, OTLP/HTTP span export, `/metrics`, and the diagnostic bundle.
