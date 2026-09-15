//! Bounded diagnostic bundle and Prometheus-compatible metrics.

use serde::Serialize;

use super::redact::redact_text;
use super::trace::{JsonLog, Span};

/// Default bundle size cap (bytes of JSON).
pub const DEFAULT_BUNDLE_LIMIT: usize = 64 * 1024;

/// In-process counters exported as Prometheus text.
#[derive(Debug, Default, Clone)]
pub struct MetricsRegistry {
    rows: Vec<(String, String, u64)>,
}

impl MetricsRegistry {
    /// Increment a labelled counter.
    pub fn incr(&mut self, name: &str, label: &str) {
        let name = redact_text(name);
        let label = redact_text(label);
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|(existing_name, existing_label, _)| {
                existing_name == &name && existing_label == &label
            })
        {
            row.2 += 1;
            return;
        }
        self.rows.push((name, label, 1));
    }

    /// Prometheus 0.0.4 text exposition.
    #[must_use]
    pub fn prometheus(&self) -> String {
        let mut out = String::new();
        for (name, label, value) in &self.rows {
            out.push_str(&format!(
                "# TYPE {name} counter\n{name}{{label=\"{label}\"}} {value}\n"
            ));
        }
        out
    }
}

/// Exportable diagnostic bundle. Always redacted, always bounded.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticBundle {
    /// Shared correlation.
    pub correlation_id: String,
    /// Spans in order.
    pub spans: Vec<Span>,
    /// Structured logs.
    pub logs: Vec<JsonLog>,
    /// Prometheus text.
    pub metrics: String,
    /// Whether the bundle hit the size cap.
    pub truncated: bool,
}

impl DiagnosticBundle {
    /// Build a bundle, dropping tail logs/spans if over `max_bytes`.
    #[must_use]
    pub fn export(
        correlation_id: impl Into<String>,
        spans: Vec<Span>,
        logs: Vec<JsonLog>,
        metrics: &MetricsRegistry,
        max_bytes: usize,
    ) -> Self {
        let correlation_id = correlation_id.into();
        let metrics_text = redact_text(&metrics.prometheus());
        let mut bundle = Self {
            correlation_id,
            spans,
            logs,
            metrics: metrics_text,
            truncated: false,
        };
        while bundle.encoded_len() > max_bytes
            && (!bundle.logs.is_empty() || !bundle.spans.is_empty())
        {
            bundle.truncated = true;
            if bundle.logs.len() > bundle.spans.len() {
                bundle.logs.pop();
            } else if !bundle.spans.is_empty() {
                bundle.spans.pop();
            } else {
                break;
            }
        }
        bundle
    }

    fn encoded_len(&self) -> usize {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX)
    }

    /// JSON bytes, already redacted.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}
