//! Redaction of a webhook payload (DOMAIN.md §13.5).
//!
//! A webhook leaves the tenant, so its body is the RuntimeEvent envelope with three things
//! taken out, in this order:
//!
//! * an object that declares `data_classes` containing a protected class keeps only its
//!   identifiers, `kind`/`type`/`status` and the `data_classes` themselves;
//! * the value under a secret-named key is replaced whatever its shape;
//! * every remaining string is scrubbed of credential-shaped tokens, secret canaries and
//!   email addresses ([`crate::observability::redact_text`], the same scrubber logs use).
//!
//! Redaction removes; it never adds a field, so a consumer's parser sees the envelope shape.

use serde_json::{Map, Value};

use crate::observability::redact_text;
use crate::policy::DataClass;

/// What replaces a secret-named value.
pub const REDACTED_SECRET: &str = "[REDACTED:secret]";
/// What replaces a field of an object that carries a protected data class.
pub const REDACTED_PROTECTED: &str = "[REDACTED:protected]";

/// Key segments that name a secret. A key matches when one of its `_`/`-`-separated
/// segments is one of these, so `access_token` and `client_secret` match while
/// `input_tokens` (a usage count) does not.
const SECRET_SEGMENTS: &[&str] = &[
    "secret",
    "secrets",
    "password",
    "passphrase",
    "token",
    "authorization",
    "credential",
    "credentials",
    "cookie",
    "cookies",
];

/// Whole keys that name a secret without a matching segment.
const SECRET_KEYS: &[&str] = &["api_key", "apikey", "private_key", "access_key"];

/// Whether a key names a secret.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_KEYS.contains(&lower.as_str())
        || lower
            .split(['_', '-', '.'])
            .any(|segment| SECRET_SEGMENTS.contains(&segment))
}

fn declares_protected(map: &Map<String, Value>) -> bool {
    map.get("data_classes")
        .and_then(Value::as_array)
        .is_some_and(|classes| {
            classes.iter().filter_map(Value::as_str).any(|class| {
                DataClass::parse(class.to_string()).is_ok_and(|class| class.is_protected())
            })
        })
}

fn kept_under_protection(key: &str) -> bool {
    key == "id"
        || key.ends_with("_id")
        || key.ends_with("_ids")
        || matches!(key, "kind" | "type" | "status" | "data_classes")
}

/// The redacted copy of a payload.
#[must_use]
pub fn redact_payload(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let protected = declares_protected(map);
            let mut out = Map::with_capacity(map.len());
            for (key, inner) in map {
                let redacted = if protected && !kept_under_protection(key) {
                    Value::String(REDACTED_PROTECTED.to_string())
                } else if is_secret_key(key) {
                    Value::String(REDACTED_SECRET.to_string())
                } else {
                    redact_payload(inner)
                };
                out.insert(key.clone(), redacted);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_payload).collect()),
        Value::String(text) => Value::String(redact_text(text)),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn secret_keys_match_by_segment() {
        for key in [
            "token",
            "access_token",
            "client_secret",
            "Authorization",
            "api_key",
            "set-cookie",
            "db.password",
        ] {
            assert!(is_secret_key(key), "{key}");
        }
        for key in [
            "input_tokens",
            "output_tokens",
            "tokenizer",
            "status",
            "secretary_name",
        ] {
            assert!(!is_secret_key(key), "{key}");
        }
    }

    #[test]
    fn a_payload_loses_secrets_protected_fields_and_credential_shaped_text() {
        let redacted = redact_payload(&json!({
            "event_id": "evt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
            "payload": {
                "text": "mail ada@example.com with Bearer abc.def and qncy_test_canary_not_for_prod",
                "client_secret": {"nested": "anything"},
                "usage": {"input_tokens": 12},
                "artifact": {
                    "artifact_id": "art_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
                    "data_classes": ["pii"],
                    "title": "Payroll for Ada",
                    "status": "ready"
                },
                "public": {"data_classes": ["internal"], "title": "Roadmap"}
            }
        }));
        let text = redacted.to_string();
        for leaked in [
            "ada@example.com",
            "abc.def",
            "qncy_test_canary_not_for_prod",
            "anything",
            "Payroll",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        assert_eq!(redacted["payload"]["usage"]["input_tokens"], 12);
        assert_eq!(
            redacted["payload"]["artifact"]["artifact_id"],
            "art_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"
        );
        assert_eq!(redacted["payload"]["artifact"]["status"], "ready");
        assert_eq!(redacted["payload"]["artifact"]["title"], REDACTED_PROTECTED);
        assert_eq!(redacted["payload"]["public"]["title"], "Roadmap");
        assert_eq!(redacted["event_id"], "evt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE");
    }
}
