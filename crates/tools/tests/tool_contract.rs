//! Tool contract conformance: the registry covers the generated catalog, arguments are
//! validated strictly before dispatch, derivations are deterministic, and exposure is
//! capability-filtered (DOMAIN.md §7.4, §7.5).

use chrono::{Duration, Utc};
use quansio_capability::projection::CapabilityProjection;
use quansio_capability::{EffectClass, Grant, SubjectKind, Tier};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_tools::call::CallContext;
use quansio_tools::{plan_call, ToolError, ToolHost, ToolRegistry};
use serde_json::{json, Value};

/// The DOMAIN.md §7.1 tiers this test needs. The server-side turn-loop test asserts that
/// the real taxonomy and the shipped registry agree, so this table cannot drift unnoticed.
const TIERS: &[(&str, u8)] = &[
    ("read.internal", 0),
    ("read.external", 0),
    ("fs.write.workspace", 1),
    ("process.exec.sandboxed", 1),
    ("content.upload", 1),
    ("memory.write", 1),
    ("network.egress.new_destination", 2),
    ("record.create", 2),
    ("fs.write.host", 3),
    ("computer.input.privileged", 3),
];

fn tier_of(class: &EffectClass) -> Option<Tier> {
    TIERS
        .iter()
        .find(|(name, _)| *name == class.as_str())
        .and_then(|(_, tier)| Tier::new(*tier).ok())
}

fn project(grants: Vec<Grant>) -> CapabilityProjection {
    let mut generator = UlidGenerator::new();
    let id = CanonicalId::generate(Prefix::CapabilityProjection, &mut generator);
    let now = Utc::now();
    CapabilityProjection::from_parts(
        id,
        quansio_capability::ProjectionSubject::new(SubjectKind::Run, "run_test"),
        Vec::new(),
        grants,
        now,
        Some(now + Duration::hours(1)),
    )
    .expect("projection")
}

fn grant(effect_class: &str, kind: &str, selector: &str) -> Grant {
    Grant::from_json(&json!({
        "effect_class": effect_class,
        "resource": { "kind": kind, "selector": selector },
        "constraints": {},
    }))
    .expect("grant")
}

fn context() -> CallContext<'static> {
    CallContext {
        workspace_id: "ws_test",
        run_id: Some("run_test"),
        fs_root: Some("/work/root"),
        own_branch: Some("feature/x"),
    }
}

#[test]
fn builtin_registry_covers_the_generated_tool_catalog() {
    let registry = ToolRegistry::builtin();
    let catalog: Value =
        serde_yaml::from_str(include_str!("../../../schemas/catalog/tools.yaml")).expect("catalog");
    let names: Vec<String> = catalog["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|value| value.as_str().expect("tool name").to_string())
        .collect();
    registry
        .check_catalog(&names)
        .expect("registry and catalog agree");
    assert_eq!(
        registry.families().len(),
        3,
        "three reserved catalog families"
    );
    assert!(
        !registry.is_empty(),
        "the builtin registry declares callable tools"
    );
}

#[test]
fn unknown_tool_is_rejected_before_dispatch() {
    let registry = ToolRegistry::builtin();
    let error = plan_call(
        registry,
        "fs.teleport",
        None,
        &json!({ "path": "/work/root/a" }),
        &tier_of,
        &context(),
    )
    .expect_err("unregistered tool");
    assert!(matches!(error, ToolError::UnknownTool { .. }));
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
}

#[test]
fn reserved_family_names_are_not_callable() {
    let registry = ToolRegistry::builtin();
    for name in ["connector.<id>.<op>", "connector.github.create_issue"] {
        let error = plan_call(registry, name, None, &json!({}), &tier_of, &context())
            .expect_err("family is not callable");
        assert!(matches!(error, ToolError::UnknownTool { .. }), "{name}");
    }
}

#[test]
fn unknown_argument_field_is_rejected() {
    let registry = ToolRegistry::builtin();
    let error = plan_call(
        registry,
        "fs.read",
        None,
        &json!({ "path": "/work/root/a", "extra": true }),
        &tier_of,
        &context(),
    )
    .expect_err("unknown field");
    assert!(matches!(error, ToolError::InvalidArguments { .. }));
}

#[test]
fn missing_and_ill_typed_arguments_are_rejected() {
    let registry = ToolRegistry::builtin();
    let missing = plan_call(registry, "fs.read", None, &json!({}), &tier_of, &context())
        .expect_err("missing path");
    assert!(matches!(missing, ToolError::InvalidArguments { .. }));

    let ill_typed = plan_call(
        registry,
        "fs.read",
        None,
        &json!({ "path": 7 }),
        &tier_of,
        &context(),
    )
    .expect_err("path must be a string");
    assert!(matches!(ill_typed, ToolError::InvalidArguments { .. }));

    let unregistered_version = plan_call(
        registry,
        "fs.read",
        Some(9),
        &json!({ "path": "/work/root/a" }),
        &tier_of,
        &context(),
    )
    .expect_err("version 9 does not exist");
    assert!(matches!(
        unregistered_version,
        ToolError::UnknownToolVersion { .. }
    ));
}

#[test]
fn derived_effect_class_follows_the_target_root() {
    let registry = ToolRegistry::builtin();
    let inside = plan_call(
        registry,
        "fs.write",
        None,
        &json!({ "path": "/work/root/a.txt", "content": "x", "content_digest": "sha256:a" }),
        &tier_of,
        &context(),
    )
    .expect("inside the root");
    assert_eq!(inside.effect_class.as_str(), "fs.write.workspace");
    assert_eq!(inside.tier.get(), 1);

    let outside = plan_call(
        registry,
        "fs.write",
        None,
        &json!({ "path": "/etc/passwd", "content": "x", "content_digest": "sha256:a" }),
        &tier_of,
        &context(),
    )
    .expect("outside the root");
    assert_eq!(outside.effect_class.as_str(), "fs.write.host");
    assert_eq!(outside.tier.get(), 3);

    let escape = plan_call(
        registry,
        "fs.write",
        None,
        &json!({ "path": "/work/root/../../etc/passwd", "content": "x", "content_digest": "sha256:a" }),
        &tier_of,
        &context(),
    )
    .expect("traversal stays outside the root");
    assert_eq!(escape.effect_class.as_str(), "fs.write.host");
}

#[test]
fn parameter_digest_is_canonical_and_key_order_independent() {
    let registry = ToolRegistry::builtin();
    let first = plan_call(
        registry,
        "terminal.exec",
        None,
        &json!({ "command": "ls", "cwd": "/work/root" }),
        &tier_of,
        &context(),
    )
    .expect("plan");
    let reordered = plan_call(
        registry,
        "terminal.exec",
        None,
        &json!({ "cwd": "/work/root", "command": "ls" }),
        &tier_of,
        &context(),
    )
    .expect("plan");
    assert_eq!(first.params_digest, reordered.params_digest);
    assert_eq!(
        first.idempotency_key.canonical(),
        reordered.idempotency_key.canonical()
    );

    let changed = plan_call(
        registry,
        "terminal.exec",
        None,
        &json!({ "command": "ls -la", "cwd": "/work/root" }),
        &tier_of,
        &context(),
    )
    .expect("plan");
    assert_ne!(first.params_digest, changed.params_digest);
}

#[test]
fn exposure_is_capability_filtered() {
    let registry = ToolRegistry::builtin();
    let now = Utc::now();

    let narrow = project(vec![grant("read.internal", "fs", "**")]);
    let exposed = registry.exposed_to(&narrow, now);
    let names: Vec<&str> = exposed.iter().map(|tool| tool.name.as_str()).collect();
    assert!(
        names.contains(&"fs.read"),
        "read.internal/fs exposes fs.read: {names:?}"
    );
    assert!(
        !names.contains(&"terminal.exec"),
        "a read grant must not expose terminal.exec: {names:?}"
    );
    assert!(!names.contains(&"user.ask"));
    assert!(registry.is_exposed("fs.read", &narrow, now));
    assert!(!registry.is_exposed("fs.write", &narrow, now));

    let empty = project(Vec::new());
    assert!(
        registry.exposed_to(&empty, now).is_empty(),
        "an empty projection exposes nothing"
    );
}

#[test]
fn declared_hosts_and_output_bounds_come_from_the_declaration() {
    let registry = ToolRegistry::builtin();
    let plan = plan_call(
        registry,
        "fs.read",
        None,
        &json!({ "path": "/work/root/a" }),
        &tier_of,
        &context(),
    )
    .expect("plan");
    assert_eq!(plan.host, ToolHost::Qworkerd);
    assert_eq!(plan.max_output_bytes, 1_048_576);
    assert_eq!(
        plan.source_trust,
        quansio_tools::SourceTrust::UntrustedExternal
    );
    assert!(plan.cancellable);
}

#[test]
fn schemas_reject_unsupported_vocabulary_at_load_time() {
    let source = r#"
version: 1
tools:
  - name: fs.read
    version: 1
    description: test
    host: qworkerd
    source_trust: untrusted_external
    effect_class: read.internal
    resource: { kind: fs, arg: path }
    idempotency: { digest_args: [path] }
    max_output_bytes: 10
    timeout_ms: 10
    grant_templates:
      - effect_class: read.internal
        resource: { kind: fs, selector: "**" }
    input_schema:
      type: object
      additionalProperties: false
      properties:
        path: { type: string, pattern: "^/.*" }
      required: [path]
    output_schema: { type: object, additionalProperties: false, properties: {} }
"#;
    let error = ToolRegistry::from_yaml(source).expect_err("pattern is unsupported");
    assert!(matches!(
        error,
        ToolError::UnsupportedSchemaKeyword { ref keyword, .. } if keyword == "pattern"
    ));
}

#[test]
fn open_object_schemas_are_refused() {
    let source = r#"
version: 1
tools:
  - name: fs.read
    version: 1
    description: test
    host: qworkerd
    source_trust: untrusted_external
    effect_class: read.internal
    resource: { kind: fs, arg: path }
    idempotency: { digest_args: [path] }
    max_output_bytes: 10
    timeout_ms: 10
    grant_templates:
      - effect_class: read.internal
        resource: { kind: fs, selector: "**" }
    input_schema: { type: object, properties: { path: { type: string } } }
    output_schema: { type: object, additionalProperties: false, properties: {} }
"#;
    let error = ToolRegistry::from_yaml(source).expect_err("open object");
    assert!(matches!(error, ToolError::MalformedDeclaration { .. }));
}
