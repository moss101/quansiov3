//! Layered settings merge (APP-002).
//!
//! Layers, most-specific last: platform defaults, tenant, workspace, user. Security
//! controls never widen: a deny in any layer wins, and numeric limits take the minimum.

use serde_json::{json, Map, Value};

/// Platform defaults. Unsigned updates and sending clipboard to a model are off.
pub fn platform_defaults() -> Value {
    json!({
        "telemetry": false,
        "clipboard_to_model": false,
        "unsigned_updates": false,
        "share_analytics": false,
        "retention_days": 365,
        "theme": "system"
    })
}

/// Keys that are security controls and must merge most-restrictively.
pub const SECURITY_BOOL_DENY_WINS: &[&str] = &[
    "telemetry",
    "clipboard_to_model",
    "unsigned_updates",
    "share_analytics",
];

/// Numeric security limits: the smallest value wins.
pub const SECURITY_MIN_WINS: &[&str] = &["retention_days"];

/// Merge layers. Later layers override non-security keys; security keys never widen.
#[must_use]
pub fn merge_settings(layers: &[Value]) -> Value {
    let mut out: Map<String, Value> = Map::new();
    for layer in layers {
        let Some(object) = layer.as_object() else {
            continue;
        };
        for (key, value) in object {
            if SECURITY_BOOL_DENY_WINS.contains(&key.as_str()) {
                let incoming = value.as_bool().unwrap_or(false);
                let current = out.get(key).and_then(Value::as_bool).unwrap_or(true);
                out.insert(key.clone(), Value::Bool(current && incoming));
            } else if SECURITY_MIN_WINS.contains(&key.as_str()) {
                let incoming = value.as_u64().unwrap_or(u64::MAX);
                let current = out.get(key).and_then(Value::as_u64).unwrap_or(u64::MAX);
                out.insert(key.clone(), json!(current.min(incoming)));
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

/// Merge platform, tenant, workspace and user layers in that order.
#[must_use]
pub fn effective_settings(tenant: &Value, workspace: &Value, user: &Value) -> Value {
    merge_settings(&[
        platform_defaults(),
        tenant.clone(),
        workspace.clone(),
        user.clone(),
    ])
}
