# OPS-003 — observability and redacted diagnostics

**What existed** (reset 2026-09-19). Model types in all three planes that nothing used:

- Rust spans were plain structs; the metrics registry was never incremented; the bundle was
  never served.
- The Python mirror types had no callers.
- The desktop bundle had no frames.

The server logged with `eprintln!`, and no trace context crossed a process boundary.

**What a run's trace is now.**

- **Desktop.** The desktop starts the trace. Every `ServerClient` call sends a fresh W3C
  `traceparent` (the same one across an auth-refresh retry) and records a redacted frame
  (method, path, status, trace id; never a body).
- **Server.** A route middleware continues the incoming `traceparent`, or starts a trace for a
  missing or malformed one. It runs the request inside that span (a Tokio task-local), answers
  with its own `traceparent`, and counts request metrics.
- **Runtime spans.** Within that span, the runtime opens a span each for the turn, each model
  call (with model-call metrics by provider and stop reason) and each tool dispatch (by tool and
  outcome; names a model invents collapse to `unregistered`).
- **Intelligence RPCs.** Every RPC is a span whose `traceparent` goes in the gRPC metadata.
- **The plane.** The Python plane continues it: each RPC is an `intelligence.handle` span with
  the caller's tenant and correlation id, logged as one JSON line.
- **Redaction.** Every line in both processes is redacted as a whole before it is written:
  secret canaries (`QUANSIO_TEST_SECRET_CANARY` plus the default), credential shapes and emails.
- **Export and endpoints.**
  - Spans go to the operator's collector over OTLP/HTTP (JSON encoding) when
    `QUANSIO_OTLP_ENDPOINT` is set. Export is batched and non-blocking, drops when full, and
    redacts attribute values.
  - `GET /metrics` serves Prometheus text to the `QUANSIO_METRICS_TOKEN` bearer only.
  - `GET /v1/diagnostics/bundle` gives tenant administrators the tenant's recent redacted log
    lines from an in-memory ring, bounded to 64 KiB. It is redacted again as a whole, with no
    store.
  - The desktop's `exportDiagnostics()` merges that bundle with its frames, and the renderer
    reaches it through `diagnostics.export`.
- **Startup.** Startup and background notices are JSON events too; stderr is one JSON object per
  line.

No new dependency in any plane. Contract (additive, same commits):

- DOMAIN §14: the `/diagnostics/bundle` read projection.
- DOMAIN §15: a tracing note (`traceparent`, `/metrics`, OTLP).

**Acceptance:**

- *"A run can be traced end-to-end across service boundaries."* In
  `crates/server/tests/observability.rs`, one chat turn runs through the real composition and
  the real Python process, starting from a desktop-style `traceparent`:
  - The server's `http.request` span is the child of the desktop span. `runtime.turn` is the
    child of `http.request`. Both `runtime.model_call` spans and `runtime.tool_call` (`fs.read`)
    are children of the turn. Each `intelligence.rpc` (2× BuildContext, 2× FulfillModel) is a
    child of its model call.
  - In the plane's own log, each of the 4 `intelligence.handle` spans is the child of the
    runtime RPC span with the same method.
  - The same trace reached an in-test OTLP/HTTP collector, carrying all five runtime span names.
  - Plane-side unit tests are in `python/tests/intelligence/test_tracing.py`. Desktop tests are
    in `apps/desktop/tests/trace.test.ts`: a traceparent on every call, a stable retry, redacted
    frames, and the merged, bounded export.
- *"Secret canary values do not appear in logs/traces."* The canary is used as the provider
  credential and in the message content. A call whose correlation id carries it makes both
  processes actually try to log it; the test sees it redacted in the runtime log, the bundle and
  the plane log. It is absent from the runtime logs, the intelligence logs, the exported spans,
  the metrics and the diagnostic bundle.
- *Diagnostic bundle test.* It is bounded, only this tenant's lines are included, a member gets
  403, and the owner gets the run's spans. Metrics give 401 without the token, with the expected
  series with it.

**Mutation checks** (`mutation-checks.log`). Each of these ten mutants compiles and fails its
suite:

1. RPC traceparent not sent;
2. incoming traceparent ignored;
3. metrics open;
4. runtime log unredacted;
5. bundle not tenant-scoped;
6. spans not exported;
7. turn not a scope;
8. the plane ignores the traceparent;
9. the plane's log unredacted;
10. the desktop sends no traceparent.
