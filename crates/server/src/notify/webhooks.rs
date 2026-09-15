//! Outbound webhooks (APP-015, DOMAIN.md §13.5).
//!
//! Delivery is at-least-once with stable `event_id`s. Payloads are tenant-scoped,
//! redacted, and only sent when egress allows and the kill switch is off.

use serde_json::{json, Value};

/// A subscription filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookSubscription {
    /// `whk_` id.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Optional workspace scope.
    pub workspace_id: Option<String>,
    /// Destination URL.
    pub url: String,
    /// Event type filters.
    pub event_types: Vec<String>,
    /// Whether delivery is disabled.
    pub disabled: bool,
}

/// Decide whether an event may be delivered.
pub fn may_deliver(
    sub: &WebhookSubscription,
    event_tenant: &str,
    event_workspace: Option<&str>,
    event_type: &str,
    egress_allowed: bool,
    kill_switch: bool,
) -> bool {
    if kill_switch || sub.disabled || !egress_allowed {
        return false;
    }
    if sub.tenant_id != event_tenant {
        return false;
    }
    if let Some(workspace) = &sub.workspace_id {
        if event_workspace != Some(workspace.as_str()) {
            return false;
        }
    }
    sub.event_types.is_empty() || sub.event_types.iter().any(|t| t == event_type)
}

/// At-least-once: a consumer dedupes on event_id.
#[must_use]
pub fn already_delivered(delivered: &[String], event_id: &str) -> bool {
    delivered.iter().any(|id| id == event_id)
}

/// Redact secrets and protected classes from a payload.
#[must_use]
pub fn redact(payload: &Value) -> Value {
    match payload {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if is_secret_key(key) {
                    out.insert(key.clone(), json!("[redacted]"));
                } else {
                    out.insert(key.clone(), redact(value));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("password")
        || lower.contains("token")
        || lower.contains("authorization")
        || lower == "api_key"
}
