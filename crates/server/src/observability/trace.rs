//! Correlated traces and structured JSON logs.

use serde::Serialize;
use serde_json::json;

use super::redact::redact_text;

/// Services a span may name. Closed so a typo cannot invent a plane.
pub const SERVICES: [&str; 3] = ["runtime", "intelligence", "desktop"];

/// Correlation context propagated across service boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Correlation {
    /// Shared id for the whole run journey.
    pub correlation_id: String,
    /// Optional run the journey belongs to.
    pub run_id: Option<String>,
    /// Tenant scope.
    pub tenant_id: String,
    /// Plane emitting this hop.
    pub service: String,
}

impl Correlation {
    /// Start a journey on the runtime plane.
    #[must_use]
    pub fn start(
        correlation_id: impl Into<String>,
        tenant_id: impl Into<String>,
        run_id: Option<String>,
    ) -> Self {
        Self {
            correlation_id: correlation_id.into(),
            run_id,
            tenant_id: tenant_id.into(),
            service: "runtime".to_string(),
        }
    }

    /// Refuse an unknown service name.
    #[must_use]
    pub fn valid_service(service: &str) -> bool {
        SERVICES.contains(&service)
    }
}

/// Continue the same correlation onto another plane.
///
/// # Errors
/// Returns a message when `service` is not a known plane.
pub fn continue_trace(parent: &Correlation, service: &str) -> Result<Correlation, String> {
    if !Correlation::valid_service(service) {
        return Err(format!("{service} is not a Quansio plane"));
    }
    Ok(Correlation {
        correlation_id: parent.correlation_id.clone(),
        run_id: parent.run_id.clone(),
        tenant_id: parent.tenant_id.clone(),
        service: service.to_string(),
    })
}

/// One span in the correlated trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    /// Span identity.
    pub span_id: String,
    /// Parent span, when this is not the root.
    pub parent_span_id: Option<String>,
    /// Shared correlation.
    pub correlation_id: String,
    /// Plane.
    pub service: String,
    /// Span name.
    pub name: String,
}

impl Span {
    /// Open a span, redacting the name.
    #[must_use]
    pub fn open(
        correlation: &Correlation,
        span_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            span_id: span_id.into(),
            parent_span_id: None,
            correlation_id: correlation.correlation_id.clone(),
            service: correlation.service.clone(),
            name: redact_text(&name.into()),
        }
    }

    /// Child span on the same correlation, possibly another service.
    #[must_use]
    pub fn child(
        &self,
        correlation: &Correlation,
        span_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            span_id: span_id.into(),
            parent_span_id: Some(self.span_id.clone()),
            correlation_id: correlation.correlation_id.clone(),
            service: correlation.service.clone(),
            name: redact_text(&name.into()),
        }
    }
}

/// One structured log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsonLog {
    /// Level.
    pub level: String,
    /// Correlation.
    pub correlation_id: String,
    /// Plane.
    pub service: String,
    /// Redacted message.
    pub msg: String,
}

impl JsonLog {
    /// Emit a JSON log; the message is redacted first.
    #[must_use]
    pub fn emit(correlation: &Correlation, level: &str, msg: &str) -> Self {
        Self {
            level: level.to_string(),
            correlation_id: correlation.correlation_id.clone(),
            service: correlation.service.clone(),
            msg: redact_text(msg),
        }
    }

    /// Wire JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        json!({
            "level": self.level,
            "correlation_id": self.correlation_id,
            "service": self.service,
            "msg": self.msg,
        })
        .to_string()
    }
}
