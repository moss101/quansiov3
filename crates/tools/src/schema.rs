//! Strict, fail-closed JSON Schema validation for tool arguments (DOMAIN.md §7.4).
//!
//! The Tool contract requires `additionalProperties = false` and a *strict* reading of
//! `input_schema`. Rather than depend on a general-purpose JSON Schema engine, this module
//! implements the exact vocabulary the Tool Registry is allowed to use and **refuses a
//! declaration that uses anything else** ([`check_vocabulary`]). A schema therefore can
//! never be silently under-enforced: unknown keywords are a load-time error, not an
//! ignored annotation.
//!
//! Supported keywords: `type`, `properties`, `required`, `additionalProperties: false`,
//! `items`, `minItems`, `maxItems`, `minLength`, `maxLength`, `minimum`, `maximum`,
//! `enum`, `const`, plus the annotations `title`, `description`, `default`.

use serde_json::{Map, Value};

use crate::error::ToolError;

/// The complete keyword vocabulary a tool schema may use.
pub const SUPPORTED_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "enum",
    "const",
    "title",
    "description",
    "default",
];

/// The JSON types this validator understands.
const SUPPORTED_TYPES: &[&str] = &[
    "object", "array", "string", "integer", "number", "boolean", "null",
];

/// One instance-level schema violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    /// Instance location: `""` at the root, `.name` for an object property, `[0]` for an
    /// array item.
    pub path: String,
    /// What the schema required.
    pub detail: String,
}

impl SchemaViolation {
    fn new(path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            detail: detail.into(),
        }
    }
}

/// Reject a schema that uses vocabulary outside [`SUPPORTED_KEYWORDS`].
///
/// Every object-typed node must declare `additionalProperties: false`, so unknown tool
/// argument fields are always rejected.
///
/// # Errors
/// Returns [`ToolError::UnsupportedSchemaKeyword`] or
/// [`ToolError::MalformedDeclaration`] for the first offending node.
pub fn check_vocabulary(tool: &str, schema: &Value) -> Result<(), ToolError> {
    let Some(object) = schema.as_object() else {
        return Err(ToolError::MalformedDeclaration {
            tool: tool.to_string(),
            detail: "a schema must be a JSON object".to_string(),
        });
    };
    for (keyword, value) in object {
        if !SUPPORTED_KEYWORDS.contains(&keyword.as_str()) {
            return Err(ToolError::UnsupportedSchemaKeyword {
                tool: tool.to_string(),
                keyword: keyword.clone(),
            });
        }
        match keyword.as_str() {
            "type" => check_type(tool, value)?,
            "properties" => {
                let Some(properties) = value.as_object() else {
                    return Err(malformed(tool, "properties must be an object"));
                };
                for (_, subschema) in properties {
                    check_vocabulary(tool, subschema)?;
                }
            }
            "items" => {
                if value.as_object().is_none() {
                    return Err(malformed(tool, "items must be a schema object"));
                }
                check_vocabulary(tool, value)?;
            }
            "required" => {
                if !value
                    .as_array()
                    .is_some_and(|items| items.iter().all(Value::is_string))
                {
                    return Err(malformed(tool, "required must be an array of strings"));
                }
            }
            "additionalProperties" => {
                if value != &Value::Bool(false) {
                    return Err(malformed(tool, "additionalProperties must be false"));
                }
            }
            "enum" if !value.is_array() => {
                return Err(malformed(tool, "enum must be an array"));
            }
            "minItems" | "maxItems" | "minLength" | "maxLength" if value.as_u64().is_none() => {
                return Err(malformed(tool, "a bound must be a non-negative integer"));
            }
            "minimum" | "maximum" if !value.is_number() => {
                return Err(malformed(tool, "a bound must be a number"));
            }
            _ => {}
        }
    }

    // Object-typed nodes must close their property set.
    let is_object = object.get("type").and_then(Value::as_str) == Some("object")
        || object.contains_key("properties");
    if is_object && object.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Err(malformed(
            tool,
            "an object schema must set additionalProperties: false",
        ));
    }
    Ok(())
}

fn malformed(tool: &str, detail: &str) -> ToolError {
    ToolError::MalformedDeclaration {
        tool: tool.to_string(),
        detail: detail.to_string(),
    }
}

fn check_type(tool: &str, value: &Value) -> Result<(), ToolError> {
    let names: Vec<&str> = match value {
        Value::String(name) => vec![name.as_str()],
        Value::Array(items) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(name) => names.push(name),
                    None => return Err(malformed(tool, "type entries must be strings")),
                }
            }
            names
        }
        _ => {
            return Err(malformed(
                tool,
                "type must be a string or an array of strings",
            ))
        }
    };
    for name in names {
        if !SUPPORTED_TYPES.contains(&name) {
            return Err(malformed(
                tool,
                &format!("type {name:?} is not one of {}", SUPPORTED_TYPES.join(", ")),
            ));
        }
    }
    Ok(())
}

/// Validate one argument object against a tool's `input_schema`.
///
/// Returns every violation found, in a deterministic traversal order; an empty result
/// means the instance satisfied the schema.
#[must_use]
pub fn validate_instance(schema: &Value, instance: &Value) -> Vec<SchemaViolation> {
    let mut violations = Vec::new();
    validate_at(schema, instance, "", &mut violations);
    violations
}

/// The first violation a strict reading of the schema would report, if any.
#[must_use]
pub fn first_violation(schema: &Value, instance: &Value) -> Option<SchemaViolation> {
    validate_instance(schema, instance).into_iter().next()
}

fn validate_at(schema: &Value, instance: &Value, path: &str, out: &mut Vec<SchemaViolation>) {
    let Some(object) = schema.as_object() else {
        return;
    };

    if let Some(constant) = object.get("const") {
        if instance != constant {
            out.push(SchemaViolation::new(
                path,
                "must equal the declared constant",
            ));
            return;
        }
    }
    if let Some(Value::Array(options)) = object.get("enum") {
        if !options.contains(instance) {
            out.push(SchemaViolation::new(
                path,
                "is not one of the declared values",
            ));
            return;
        }
    }
    if let Some(types) = object.get("type") {
        if !matches_type(types, instance) {
            out.push(SchemaViolation::new(
                path,
                format!("must be of type {}", describe_types(types)),
            ));
            return;
        }
    }

    if let Some(instance_object) = instance.as_object() {
        if let Some(Value::Array(required)) = object.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !instance_object.contains_key(name) {
                    out.push(SchemaViolation::new(
                        child(path, name),
                        "is a required field",
                    ));
                }
            }
        }
        if let Some(Value::Object(properties)) = object.get("properties") {
            for (name, value) in instance_object {
                match properties.get(name) {
                    Some(subschema) => validate_at(subschema, value, &child(path, name), out),
                    None => {
                        if object.get("additionalProperties") == Some(&Value::Bool(false)) {
                            out.push(SchemaViolation::new(
                                child(path, name),
                                "is not a declared field",
                            ));
                        }
                    }
                }
            }
        }
    }

    if let (Some(items), Some(subschema)) = (instance.as_array(), object.get("items")) {
        push_count_violations(object, items, path, out);
        for (index, item) in items.iter().enumerate() {
            validate_at(subschema, item, &index_path(path, index), out);
        }
    }

    if let Some(text) = instance.as_str() {
        check_length(
            object,
            "minLength",
            text.chars().count(),
            path,
            out,
            |bound, actual| actual < bound,
        );
        check_length(
            object,
            "maxLength",
            text.chars().count(),
            path,
            out,
            |bound, actual| actual > bound,
        );
    }

    if let Some(number) = instance.as_f64() {
        if let Some(minimum) = object.get("minimum").and_then(Value::as_f64) {
            if number < minimum {
                out.push(SchemaViolation::new(path, format!("must be >= {minimum}")));
            }
        }
        if let Some(maximum) = object.get("maximum").and_then(Value::as_f64) {
            if number > maximum {
                out.push(SchemaViolation::new(path, format!("must be <= {maximum}")));
            }
        }
    }
}

fn push_count_violations(
    schema: &Map<String, Value>,
    items: &[Value],
    path: &str,
    out: &mut Vec<SchemaViolation>,
) {
    if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64) {
        if (items.len() as u64) < minimum {
            out.push(SchemaViolation::new(
                path,
                format!("must hold at least {minimum} items"),
            ));
        }
    }
    if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64) {
        if (items.len() as u64) > maximum {
            out.push(SchemaViolation::new(
                path,
                format!("must hold at most {maximum} items"),
            ));
        }
    }
}

fn check_length(
    schema: &Map<String, Value>,
    keyword: &str,
    actual: usize,
    path: &str,
    out: &mut Vec<SchemaViolation>,
    violates: impl Fn(usize, usize) -> bool,
) {
    if let Some(bound) = schema.get(keyword).and_then(Value::as_u64) {
        let bound = bound as usize;
        if violates(bound, actual) {
            out.push(SchemaViolation::new(
                path,
                format!("must satisfy {keyword} {bound}"),
            ));
        }
    }
}

fn child(path: &str, name: &str) -> String {
    format!("{path}.{name}")
}

fn index_path(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn matches_type(types: &Value, instance: &Value) -> bool {
    match types {
        Value::String(name) => matches_named_type(name, instance),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| matches_named_type(name, instance)),
        _ => true,
    }
}

fn describe_types(types: &Value) -> String {
    match types {
        Value::String(name) => name.clone(),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" | "),
        _ => "the declared type".to_string(),
    }
}

fn matches_named_type(name: &str, instance: &Value) -> bool {
    match name {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "integer" => instance.as_i64().is_some() || instance.as_u64().is_some(),
        "number" => instance.is_number(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        _ => false,
    }
}
