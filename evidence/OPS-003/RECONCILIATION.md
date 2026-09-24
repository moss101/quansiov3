# OPS-003 reconciliation — observability and redacted diagnostics

**Canonical owners** (the task's `paths`):

- `crates/server/src/observability/` for the runtime and API plane;
- `python/intelligence/observability/` for the intelligence plane;
- `apps/desktop/src/diagnostics/` for the desktop plane.

Wiring into each plane's composition root (server composition and API router, the intelligence
servicer and app, the desktop server client) happens at the call sites those owners already
have.

**Existing code (2026-09-15, reset 2026-09-19; re-reconciled 2026-09-24).** Classification:
PARTIAL — model types with nothing using them.

- Rust `trace.rs` models spans as plain structs, and nothing emits them.
- `bundle.rs` has an in-memory counter registry that nothing increments, and a bundle nothing
  serves.
- `redact.rs` redacts canaries, tokens and emails. Only the webhook redactor uses it.
- Python `correlation.py`/`redact.py` mirror the Rust types; nothing calls them.
- Desktop `bundle.ts` redacts and bounds frames that nothing produces.

The server logs with `eprintln!` (no structured logs, no correlation or trace ids, no metrics
endpoint). The intelligence servicer logs with %-formatting. No trace context crosses a process
boundary. The only correlation that does is the gRPC `ScopeContext.correlation_id`.

**Design:**

- **Trace context.** W3C `traceparent` (trace-id / parent span-id). The desktop sends one per
  command. The server continues an incoming one or starts one per request, holds it in a
  task-local for the request's whole future (API → runtime turn → model source → gateway
  client), and sends a child `traceparent` as gRPC metadata on every intelligence call. The
  plane continues it for each RPC.
- **Structured logs.** One JSON line per event, carrying `trace_id`, `span_id`,
  `parent_span_id`, `correlation_id`, `tenant_id` where known, `service`, `event` and
  `duration_ms`. Every line passes the redactor (secret canaries from
  `QUANSIO_TEST_SECRET_CANARY`, credential shapes, emails) before it is written. Message
  content is never logged.
- **Metrics.** A Prometheus text endpoint `GET /metrics`, operator-level and enabled only with
  `QUANSIO_METRICS_TOKEN` (bearer). It carries HTTP requests by route and status, model calls
  by provider and stop reason, and tool calls by tool and outcome.
- **Diagnostic bundle.** `GET /v1/diagnostics/bundle` (tenant administrators) returns this
  tenant's recent redacted log lines from an in-memory ring (no store), with the build version,
  bounded to `DEFAULT_BUNDLE_LIMIT`. The desktop exports it together with its own redacted
  frames.
- **OTLP.** When `QUANSIO_OTLP_ENDPOINT` is set, the server exports its spans over OTLP/gRPC. It
  is off by default; it is an operator-configured effect to the operator's own collector.

**Persistent state.** None: the ring buffer is in memory and metrics are process counters.

**Contracts (additive).**
- DOMAIN §14: `GET /v1/diagnostics/bundle` (read projection).
- DOMAIN §15: the error shape is unchanged; the trace id rides in logs, not in errors.
- DOMAIN §16 note on the `traceparent` header and gRPC metadata.

**Effects.** OTLP export only when configured. **Real boundaries:** none (`real_boundary=false`).

**Rollback.** Unset the environment variables; logs return to plain stderr JSON.

## Plan

- [ ] Rust:
  - `observability::context` (W3C trace context, task-local);
  - `observability::logging` (JSON tracing layer through the redacting writer, plus the ring);
  - `observability::metrics` (a global registry with Prometheus text);
  - HTTP middleware (continue or start the trace, a request span, a completion log, metrics);
  - runtime events for turns, model calls and tool calls;
  - `traceparent` on every intelligence RPC;
  - `/metrics`; `/v1/diagnostics/bundle`;
  - composition installs logging;
  - OTLP exporter behind `QUANSIO_OTLP_ENDPOINT`.
- [ ] Python: the servicer continues the trace per RPC and logs start/end JSON lines through
  the redacting formatter; the app installs the JSON log handler.
- [ ] Desktop: the server client sends a `traceparent` per command; the diagnostics export
  merges the server bundle with desktop frames, redacted and bounded.
- [ ] Tests:
  - trace propagation: desktop-style `traceparent` → server spans → the Python RPC logs share
    the trace id, with parent links, through the real process;
  - secret canary scan: the canary as a provider credential and in message content; absent
    from server logs, Python logs, the bundle and metrics;
  - diagnostic bundle: bounded, redacted, tenant-scoped, admin-only;
  - an OTLP export test against an in-test collector;
  - mutation checks.
