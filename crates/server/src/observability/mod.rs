//! Observability: correlated traces, redacted logs, Prometheus metrics, diagnostic bundles (OPS-003).
//!
//! A run is diagnosable across runtime, intelligence and desktop because every span and log
//! carries the same `correlation_id`. Secret canaries and credential-shaped values are
//! redacted before a log, trace attribute or diagnostic bundle is emitted.

mod bundle;
mod redact;
mod trace;

pub use bundle::{DiagnosticBundle, MetricsRegistry, DEFAULT_BUNDLE_LIMIT};
pub use redact::{redact_text, secret_canaries, CANARY_PLACEHOLDER, DEFAULT_SECRET_CANARY};
pub use trace::{continue_trace, Correlation, JsonLog, Span, SERVICES};

/// Repository path of this module's canonical owner.
pub const OBSERVABILITY_OWNER: &str = "crates/server/src/observability";
