//! The bounded, redacted diagnostic bundle a tenant administrator exports.
//!
//! It is read from the logger's in-memory ring — no store — and holds only lines tagged with the
//! requesting tenant, newest kept when the bound is reached. Every line was redacted when it was
//! written; the bundle is redacted again as a whole before it leaves, so nothing a later change
//! lets into a line can leave through the bundle either.

use serde::Serialize;
use serde_json::Value;

use super::log::recent_for_tenant;
use super::redact::redact_text;

/// Default bundle size cap (bytes of JSON).
pub const DEFAULT_BUNDLE_LIMIT: usize = 64 * 1024;

/// An exportable diagnostic bundle.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticBundle {
    /// The server build that produced it.
    pub version: String,
    /// When it was produced (RFC 3339).
    pub generated_at: String,
    /// The tenant it is scoped to.
    pub tenant_id: String,
    /// Recent log lines for the tenant, oldest first.
    pub logs: Vec<Value>,
    /// Whether older lines were left out to respect the bound.
    pub truncated: bool,
}

impl DiagnosticBundle {
    /// The tenant's recent lines, bounded to `max_bytes` of JSON.
    #[must_use]
    pub fn for_tenant(tenant_id: &str, max_bytes: usize) -> Self {
        let lines = recent_for_tenant(tenant_id);
        let mut bundle = Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            tenant_id: tenant_id.to_string(),
            logs: lines
                .iter()
                .map(|line| {
                    serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.clone()))
                })
                .collect(),
            truncated: false,
        };
        while bundle.to_json().len() > max_bytes && !bundle.logs.is_empty() {
            bundle.logs.remove(0);
            bundle.truncated = true;
        }
        bundle
    }

    /// The bundle as JSON, redacted as a whole.
    #[must_use]
    pub fn to_json(&self) -> String {
        redact_text(&serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string()))
    }
}
