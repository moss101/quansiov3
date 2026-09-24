//! Observability: trace context, structured redacted logs, spans, metrics, OTLP export and the
//! diagnostic bundle (OPS-003).
//!
//! A run is diagnosable across the desktop, the runtime and the intelligence plane because the
//! same W3C trace id travels with it ([`context`]): every log line and span of the run carries it,
//! and every call to the intelligence plane passes it on. Secret canaries and credential-shaped
//! values are redacted before a log line, span attribute, metric label or diagnostic bundle leaves
//! the process ([`redact_text`]).

pub mod bundle;
pub mod context;
pub mod log;
pub mod metrics;
pub mod otlp;
mod redact;

pub use bundle::{DiagnosticBundle, DEFAULT_BUNDLE_LIMIT};
pub use context::TraceContext;
pub use log::{event, install, notice, Level, LogConfig, LogSink, Span, DEFAULT_RING_CAPACITY};
pub use otlp::OtlpExporter;
pub use redact::{redact_text, secret_canaries, CANARY_PLACEHOLDER, DEFAULT_SECRET_CANARY};

/// Repository path of this module's canonical owner.
pub const OBSERVABILITY_OWNER: &str = "crates/server/src/observability";

/// The W3C header (and gRPC metadata key) that carries the trace context.
pub const TRACEPARENT: &str = "traceparent";
