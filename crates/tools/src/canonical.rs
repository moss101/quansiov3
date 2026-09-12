//! Deterministic canonical JSON for tool parameter digests (DOMAIN.md §7.5).
//!
//! Two calls with the same arguments must derive the same `params_digest`, so the
//! parameter form fed to [`quansio_core::Digest`] is canonical: object keys are sorted,
//! arrays keep their order and no insignificant whitespace is emitted.

use serde_json::{Map, Value};

/// Serialize a JSON value to its canonical form.
#[must_use]
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => write_object(map, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        other => {
            let encoded = other.to_string();
            out.push_str(&encoded);
        }
    }
}

fn write_object(map: &Map<String, Value>, out: &mut String) {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_unstable();
    out.push('{');
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let encoded_key = Value::String((*key).clone()).to_string();
        out.push_str(&encoded_key);
        out.push(':');
        write_value(&map[*key], out);
    }
    out.push('}');
}
