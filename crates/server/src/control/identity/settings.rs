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

/// Settings keys the product understands beyond the security controls.
///
/// * `execution_target`: `local_capsule` | `cloud` | `private_worker` — where a workspace's
///   Persistent Workspace Computer runs; changing it restarts the target.
/// * `model.primary`: a catalog model id (`config/models.yaml`); applied per call.
/// * `providers.<name>.credential_handle`: a `sec_` handle; never credential material.
/// * `knowledge.embedding_model`: changing it re-embeds, so the derived index rebuilds.
pub const EXECUTION_TARGETS: &[&str] = &["local_capsule", "cloud", "private_worker"];

/// A consequence of a settings change the caller must be told about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEffect {
    /// A component restarts (the reason names it).
    Restart(String),
    /// A derived index rebuilds (the reason names it).
    Reindex(String),
}

impl SettingsEffect {
    /// JSON shape for the command result.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Restart(reason) => json!({"kind": "restart", "reason": reason}),
            Self::Reindex(reason) => json!({"kind": "reindex", "reason": reason}),
        }
    }
}

/// Refuse a settings layer that would carry a raw secret or an impossible value.
///
/// # Errors
/// Returns a message naming the offending key.
pub fn validate_settings(settings: &Value) -> Result<(), String> {
    let Some(object) = settings.as_object() else {
        return Err("settings must be a JSON object".to_string());
    };
    for (key, value) in object {
        match key.as_str() {
            "execution_target" => {
                let Some(target) = value.as_str() else {
                    return Err("execution_target must be a string".to_string());
                };
                if !EXECUTION_TARGETS.contains(&target) {
                    return Err(format!(
                        "execution_target must be one of {}",
                        EXECUTION_TARGETS.join(", ")
                    ));
                }
            }
            "providers" => {
                let Some(providers) = value.as_object() else {
                    return Err("providers must be an object".to_string());
                };
                for (name, entry) in providers {
                    let Some(handle) = entry.get("credential_handle").and_then(Value::as_str)
                    else {
                        return Err(format!("providers.{name}.credential_handle is required"));
                    };
                    if !handle.starts_with("sec_") {
                        return Err(format!(
                            "providers.{name}.credential_handle must be a sec_ secret handle, never the credential"
                        ));
                    }
                    if looks_like_secret(handle) {
                        return Err(format!(
                            "providers.{name}.credential_handle looks like a raw secret"
                        ));
                    }
                }
            }
            "model" => {
                let Some(model) = value.as_object() else {
                    return Err("model must be an object".to_string());
                };
                if let Some(primary) = model.get("primary") {
                    let Some(primary) = primary.as_str() else {
                        return Err("model.primary must be a catalog model id".to_string());
                    };
                    if !primary
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    {
                        return Err("model.primary must be a catalog model id".to_string());
                    }
                }
            }
            key if SECURITY_BOOL_DENY_WINS.contains(&key) => {
                if !value.is_boolean() {
                    return Err(format!("{key} must be a boolean"));
                }
            }
            key if SECURITY_MIN_WINS.contains(&key) && value.as_u64().is_none() => {
                return Err(format!("{key} must be a non-negative integer"));
            }
            _ => {}
        }
        if let Some(text) = value.as_str() {
            if looks_like_secret(text) {
                return Err(format!(
                    "{key} looks like a raw secret; store a sec_ handle instead"
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn looks_like_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("sk-")
        || lower.starts_with("sk_live")
        || lower.starts_with("ghp_")
        || lower.starts_with("xoxb-")
        || lower.starts_with("qsk_")
        || lower.starts_with("-----begin")
}

/// The restart/reindex consequences of moving from `before` to `after`, most disruptive first.
#[must_use]
pub fn settings_effects(before: &Value, after: &Value) -> Vec<SettingsEffect> {
    let mut effects = Vec::new();
    if before.get("execution_target") != after.get("execution_target") {
        effects.push(SettingsEffect::Restart(
            "execution target changed: the workspace computer is re-provisioned".to_string(),
        ));
    }
    let embedding = |value: &Value| {
        value
            .get("knowledge")
            .and_then(|k| k.get("embedding_model"))
            .cloned()
    };
    if embedding(before) != embedding(after) {
        effects.push(SettingsEffect::Reindex(
            "embedding model changed: the derived vector index rebuilds".to_string(),
        ));
    }
    if before.get("providers") != after.get("providers") {
        effects.push(SettingsEffect::Restart(
            "provider credentials changed: the model gateway reloads its custody".to_string(),
        ));
    }
    effects
}
