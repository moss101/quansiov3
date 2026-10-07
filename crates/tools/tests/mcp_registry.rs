//! External tools in the Tool Registry (ADP-009, DOMAIN.md §7.5 "External tools").
//!
//! * a server's tools register as `mcp.<server>.<tool>` and nothing else: a name outside the namespace, a built-in's
//!   name, a declaration the runtime did not build (a built-in origin, a host other than the worker, trusted results) and
//!   a reused version with new content are all refused, and a refusal leaves the registry as it was;
//! * **projection narrowing**: an external tool is exposed to a model only when the run's projection holds a grant for the
//!   class and resource the configuration gave it. Over every subset of a pool of grants, the tools a subset exposes are
//!   inside the tools any superset exposes, a tool nobody classified (tier 4) is never exposed by a read grant, and a
//!   connector wildcard exposes exactly the tools whose class it names;
//! * **a tool list that changes is never auto-granted**: a tool added after the projection was computed is absent from
//!   that projection's view, a retired tool is exposed to nobody and a call to it is refused as unregistered, and an older
//!   version of a changed tool still resolves for a call that parked before the change;
//! * the handle gives every holder one consistent snapshot and a refresh never changes a snapshot already taken.

use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use quansio_capability::projection::CapabilityProjection;
use quansio_capability::{EffectClass, Grant, SubjectKind, Tier};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_tools::call::{CallContext, Caller};
use quansio_tools::mcp::{build_server_tools, DiscoveredTool};
use quansio_tools::{
    plan_call, McpConfig, RegistryHandle, ToolDeclaration, ToolError, ToolOrigin, ToolRegistry,
};
use serde_json::{json, Value};

fn config() -> McpConfig {
    let mut text = include_str!("../../../config/mcp.yaml").replace("servers: []", "");
    text.push_str(
        "servers:
  - name: demo
    transport: stdio
    command: /bin/true
    tools:
      read_note:
        effect_class: read.external
      send_note:
        effect_class: message.send
  - name: other
    transport: stdio
    command: /bin/true
    tools:
      read_note:
        effect_class: read.external
",
    );
    McpConfig::from_yaml(&text).expect("configuration")
}

fn offer(name: &str, properties: Value) -> DiscoveredTool {
    DiscoveredTool {
        name: name.to_string(),
        description: None,
        input_schema: json!({"type": "object", "properties": properties}),
    }
}

fn build(
    config: &McpConfig,
    server: &str,
    previous: &[&ToolDeclaration],
    offers: &[DiscoveredTool],
) -> Vec<ToolDeclaration> {
    let built = build_server_tools(
        config,
        config.server(server).expect("server"),
        offers,
        previous,
    );
    assert!(built.rejected.is_empty(), "{:?}", built.rejected);
    built.declarations
}

fn demo_offers() -> Vec<DiscoveredTool> {
    vec![
        offer("read_note", json!({"id": {"type": "string"}})),
        offer("send_note", json!({"to": {"type": "string"}})),
        offer("mystery", json!({})),
    ]
}

fn project(grants: Vec<Grant>) -> CapabilityProjection {
    let id = CanonicalId::generate(Prefix::CapabilityProjection, &mut UlidGenerator::new());
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

fn grant(class: &str, selector: &str) -> Grant {
    Grant::from_json(&json!({
        "effect_class": class,
        "resource": { "kind": "connector", "selector": selector },
        "constraints": {},
    }))
    .expect("grant")
}

fn tier_of(_class: &EffectClass) -> Option<Tier> {
    Tier::new(0).ok()
}

fn context(caller: Caller) -> CallContext<'static> {
    CallContext {
        caller,
        workspace_id: "ws_test",
        run_id: Some("run_test"),
        fs_root: None,
        own_branch: None,
    }
}

fn registry_with_demo() -> ToolRegistry {
    ToolRegistry::builtin()
        .with_server_tools("demo", build(&config(), "demo", &[], &demo_offers()))
        .expect("registers")
}

fn exposed(registry: &ToolRegistry, grants: Vec<Grant>) -> BTreeSet<String> {
    registry
        .exposed_names(&project(grants), Utc::now())
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[test]
fn external_tools_register_under_their_server_and_leave_the_builtin_tools_alone() {
    let builtin = ToolRegistry::builtin();
    let registry = registry_with_demo();
    for name in [
        "mcp.demo.read_note",
        "mcp.demo.send_note",
        "mcp.demo.mystery",
    ] {
        let declaration = registry.get(name).expect(name);
        assert!(matches!(declaration.origin, ToolOrigin::External { .. }));
    }
    assert_eq!(registry.external().count(), 3);
    for declaration in builtin.declarations() {
        assert_eq!(
            registry.get(&declaration.name),
            Some(declaration),
            "{}",
            declaration.name
        );
    }
    registry
        .check_catalog(&yaml_catalog())
        .expect("the registry still matches the generated catalog: the family covers the names");
}

fn yaml_catalog() -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Catalog {
        tools: Vec<String>,
    }
    let text = include_str!("../../../schemas/catalog/tools.yaml");
    serde_yaml::from_str::<Catalog>(text)
        .expect("catalog")
        .tools
}

#[test]
fn a_declaration_the_runtime_did_not_build_is_refused_and_the_registry_is_unchanged() {
    let builtin = ToolRegistry::builtin();
    let good = build(&config(), "demo", &[], &demo_offers());
    let before = builtin.len();

    // A name that is not mcp.<server>.<tool>.
    let mut renamed = good[0].clone();
    renamed.name = "fs.read".to_string();
    assert!(matches!(
        builtin.with_server_tools("demo", vec![renamed]),
        Err(ToolError::MalformedDeclaration { .. })
    ));
    let mut stray_namespace = good[0].clone();
    stray_namespace.name = "mcp.other.read_note".to_string();
    assert!(builtin
        .with_server_tools("demo", vec![stray_namespace])
        .is_err());

    // A built-in origin (a config/tools.yaml declaration) cannot be registered as external.
    let mut origin = good[0].clone();
    origin.origin = ToolOrigin::Builtin;
    assert!(builtin.with_external(vec![origin]).is_err());

    // The worker is the only host, and the results are untrusted whatever the declaration says.
    let mut host = good[0].clone();
    host.host = quansio_tools::ToolHost::Server;
    assert!(builtin.with_external(vec![host]).is_err());
    let mut trust = good[0].clone();
    trust.source_trust = quansio_tools::SourceTrust::TrustedSystem;
    assert!(builtin.with_external(vec![trust]).is_err());
    let mut callable = good[0].clone();
    callable.host_callable = true;
    assert!(builtin.with_external(vec![callable]).is_err());

    // Another server's tool is not this server's.
    let other = build(&config(), "other", &[], &[offer("read_note", json!({}))]);
    assert!(builtin.with_server_tools("demo", other).is_err());

    // A reused version with different content.
    let registry = builtin
        .with_server_tools("demo", good.clone())
        .expect("registers");
    let mut changed = good[0].clone();
    changed.max_output_bytes += 1;
    assert!(registry.with_server_tools("demo", vec![changed]).is_err());

    assert_eq!(builtin.len(), before, "a refusal changes nothing");
}

#[test]
fn a_tool_cannot_take_a_builtin_name() {
    // The external tools are all `mcp.*`, so the collision that matters is a *built-in* that someone later declares
    // under the namespace: it wins, and the registry refuses the external one.
    let builtin_in_namespace = ToolRegistry::from_yaml(
        r#"
version: 1
tools:
  - name: mcp.demo.read_note
    version: 1
    description: A built-in that sits in the namespace.
    host: server
    source_trust: untrusted_external
    effect_class: read.internal
    resource: { kind: fs, selector: "**" }
    idempotency: { digest_args: [key] }
    max_output_bytes: 1024
    timeout_ms: 1000
    evidence: { kinds: [], redact: [] }
    grant_templates:
      - effect_class: read.internal
        resource: { kind: fs, selector: "**" }
        constraints: {}
    input_schema: { type: object, additionalProperties: false, properties: { key: { type: string } }, required: [key] }
    output_schema: { type: object, additionalProperties: false, properties: { ok: { type: boolean } }, required: [ok] }
"#,
    )
    .expect("fixture");
    let external = build(&config(), "demo", &[], &[offer("read_note", json!({}))]);
    let error = builtin_in_namespace
        .with_server_tools("demo", external)
        .expect_err("collision");
    assert!(error.to_string().contains("built-in"), "{error}");
}

#[test]
fn a_tool_nobody_classified_is_exposed_by_no_read_grant() {
    let registry = registry_with_demo();
    let everything = [
        grant("read.external", "mcp.demo/*"),
        grant("read.internal", "*"),
        grant("message.send", "mcp.other/*"),
    ];
    let seen = exposed(&registry, everything.to_vec());
    assert!(seen.contains("mcp.demo.read_note"), "{seen:?}");
    assert!(
        !seen.contains("mcp.demo.mystery"),
        "an unclassified tool settles as tier 4, so a read grant never exposes it: {seen:?}"
    );
    assert!(
        !seen.contains("mcp.demo.send_note"),
        "a message.send grant on another server does not cover this one: {seen:?}"
    );
    // Only a grant for the class the configuration gave it (data.upload.protected) exposes the unclassified tool.
    let widened = exposed(
        &registry,
        vec![grant("data.upload.protected", "mcp.demo/mystery")],
    );
    assert_eq!(widened, BTreeSet::from(["mcp.demo.mystery".to_string()]));
}

#[test]
fn a_projection_can_only_remove_external_tools_from_the_model_view() {
    let registry = registry_with_demo();
    let pool = [
        grant("read.external", "mcp.demo/read_note"),
        grant("message.send", "mcp.demo/*"),
        grant("data.upload.protected", "*"),
        grant("read.external", "mcp.other/*"),
    ];
    let mut by_mask: Vec<(usize, BTreeSet<String>)> = Vec::new();
    for mask in 0..(1usize << pool.len()) {
        let grants: Vec<Grant> = (0..pool.len())
            .filter(|index| mask & (1 << index) != 0)
            .map(|index| pool[index].clone())
            .collect();
        by_mask.push((mask, exposed(&registry, grants)));
    }
    for (small, small_view) in &by_mask {
        for (large, large_view) in &by_mask {
            if small & large == *small {
                assert!(
                    small_view.is_subset(large_view),
                    "grants {small:b} expose {small_view:?}, which is not inside {large_view:?} exposed by {large:b}"
                );
            }
        }
    }
    let (_, empty) = &by_mask[0];
    assert!(
        empty.iter().all(|name| !name.starts_with("mcp.")),
        "no grant, no external tool: {empty:?}"
    );
    let (_, all) = by_mask.last().expect("all");
    assert!(
        all.contains("mcp.demo.read_note")
            && all.contains("mcp.demo.send_note")
            && all.contains("mcp.demo.mystery")
    );
}

#[test]
fn a_tool_added_to_a_server_after_the_projection_is_not_in_that_projections_view() {
    let handle = RegistryHandle::builtin();
    let config = config();
    handle
        .replace_server_tools(
            "demo",
            build(&config, "demo", &[], &[offer("read_note", json!({}))]),
        )
        .expect("first registration");
    let grants = vec![grant("read.external", "mcp.demo/read_note")];
    let before = exposed(&handle.current(), grants.clone());
    assert_eq!(before, BTreeSet::from(["mcp.demo.read_note".to_string()]));

    // The server adds a tool, and the configuration classifies none of its new tools as a read.
    let snapshot = handle.current();
    let previous: Vec<ToolDeclaration> =
        snapshot.external_of("demo").into_iter().cloned().collect();
    let previous_refs: Vec<&ToolDeclaration> = previous.iter().collect();
    let change = handle
        .replace_server_tools(
            "demo",
            build(
                &config,
                "demo",
                &previous_refs,
                &[
                    offer("read_note", json!({})),
                    offer("send_note", json!({})),
                    offer("shiny_new", json!({})),
                ],
            ),
        )
        .expect("refresh");
    assert_eq!(
        change.added,
        vec![
            "mcp.demo.send_note".to_string(),
            "mcp.demo.shiny_new".to_string()
        ]
    );

    let after = exposed(&handle.current(), grants);
    assert_eq!(
        after, before,
        "the same grants expose the same tools: a new tool is never auto-granted"
    );
    // ...and the snapshot taken before the refresh is exactly as it was.
    assert!(snapshot.get("mcp.demo.shiny_new").is_none());
    assert!(handle.current().get("mcp.demo.shiny_new").is_some());
}

#[test]
fn a_retired_tool_is_exposed_to_nobody_and_refused_as_unregistered() {
    let handle = RegistryHandle::builtin();
    let config = config();
    handle
        .replace_server_tools("demo", build(&config, "demo", &[], &demo_offers()))
        .expect("registers");
    // A second server's tools are untouched by a refresh of the first.
    handle
        .replace_server_tools(
            "other",
            build(&config, "other", &[], &[offer("read_note", json!({}))]),
        )
        .expect("registers");
    let snapshot = handle.current();
    let previous: Vec<ToolDeclaration> =
        snapshot.external_of("demo").into_iter().cloned().collect();
    let refs: Vec<&ToolDeclaration> = previous.iter().collect();
    let change = handle
        .replace_server_tools(
            "demo",
            build(&config, "demo", &refs, &[offer("read_note", json!({}))]),
        )
        .expect("refresh");
    assert_eq!(
        change.removed,
        vec![
            "mcp.demo.mystery".to_string(),
            "mcp.demo.send_note".to_string()
        ]
    );

    let registry = handle.current();
    assert!(registry.is_retired("mcp.demo.send_note"));
    assert!(registry.get("mcp.demo.send_note").is_none());
    assert!(
        registry.get("mcp.other.read_note").is_some(),
        "another server is untouched"
    );
    let seen = exposed(
        &registry,
        vec![
            grant("message.send", "mcp.demo/*"),
            grant("data.upload.protected", "*"),
            grant("read.external", "*"),
        ],
    );
    assert_eq!(
        seen,
        BTreeSet::from([
            "mcp.demo.read_note".to_string(),
            "mcp.other.read_note".to_string()
        ]),
        "a retired tool is exposed to nobody"
    );
    let refused = plan_call(
        &registry,
        "mcp.demo.send_note",
        None,
        &json!({"to": "x"}),
        &tier_of,
        &context(Caller::Model),
    )
    .expect_err("retired");
    assert!(
        matches!(refused, ToolError::UnknownTool { .. }),
        "{refused}"
    );

    // A tool that comes back is live again only because its server offers it again.
    let again: Vec<ToolDeclaration> = registry.external_of("demo").into_iter().cloned().collect();
    let again_refs: Vec<&ToolDeclaration> = again.iter().collect();
    handle
        .replace_server_tools("demo", build(&config, "demo", &again_refs, &demo_offers()))
        .expect("offered again");
    assert!(handle.current().get("mcp.demo.send_note").is_some());
}

#[test]
fn an_older_version_of_a_changed_tool_still_resolves_for_a_call_that_parked_before_the_change() {
    let handle = RegistryHandle::builtin();
    let config = config();
    handle
        .replace_server_tools(
            "demo",
            build(
                &config,
                "demo",
                &[],
                &[offer("send_note", json!({"to": {"type": "string"}}))],
            ),
        )
        .expect("registers");
    let snapshot = handle.current();
    let previous: Vec<ToolDeclaration> =
        snapshot.external_of("demo").into_iter().cloned().collect();
    let refs: Vec<&ToolDeclaration> = previous.iter().collect();
    handle
        .replace_server_tools(
            "demo",
            build(
                &config,
                "demo",
                &refs,
                &[offer(
                    "send_note",
                    json!({"to": {"type": "string"}, "cc": {"type": "string"}}),
                )],
            ),
        )
        .expect("changed");
    let registry = handle.current();
    assert_eq!(
        registry.get("mcp.demo.send_note").expect("latest").version,
        2
    );

    // A new call plans against the new schema: `cc` is a field now.
    let new_call = json!({"to": "a", "cc": "b"});
    let planned = plan_call(
        &registry,
        "mcp.demo.send_note",
        None,
        &new_call,
        &tier_of,
        &context(Caller::Model),
    )
    .expect("v2");
    assert_eq!(planned.version, 2);
    // A call that parked at version 1 resumes against version 1, which does not know `cc`.
    let resumed = plan_call(
        &registry,
        "mcp.demo.send_note",
        Some(1),
        &json!({"to": "a"}),
        &tier_of,
        &context(Caller::Model),
    )
    .expect("v1");
    assert_eq!(resumed.version, 1);
    assert!(plan_call(
        &registry,
        "mcp.demo.send_note",
        Some(1),
        &new_call,
        &tier_of,
        &context(Caller::Model)
    )
    .is_err());
}

#[test]
fn a_planned_external_call_derives_its_class_resource_and_digest_from_the_declaration_alone() {
    let registry = registry_with_demo();
    let planned = plan_call(
        &registry,
        "mcp.demo.send_note",
        None,
        &json!({"to": "someone"}),
        &|_| Tier::new(3).ok(),
        &context(Caller::Model),
    )
    .expect("plans");
    assert_eq!(planned.effect_class.as_str(), "message.send");
    assert_eq!(planned.resource.to_string(), "connector:mcp.demo/send_note");
    assert_eq!(planned.host, quansio_tools::ToolHost::Qworkerd);
    assert_eq!(
        planned.source_trust,
        quansio_tools::SourceTrust::UntrustedExternal
    );
    // An argument the schema does not declare is refused before anything else, as for every tool.
    let error = plan_call(
        &registry,
        "mcp.demo.send_note",
        None,
        &json!({"to": "someone", "bcc": "x"}),
        &tier_of,
        &context(Caller::Model),
    )
    .expect_err("closed");
    assert!(matches!(error, ToolError::InvalidArguments { .. }));
    // Not callable by the host: an external tool has no host path.
    assert!(matches!(
        plan_call(
            &registry,
            "mcp.demo.send_note",
            None,
            &json!({"to": "x"}),
            &tier_of,
            &context(Caller::Host)
        ),
        Err(ToolError::NotHostCallable { .. })
    ));
}

#[test]
fn a_handle_gives_one_snapshot_and_a_failed_refresh_changes_nothing() {
    let handle = RegistryHandle::builtin();
    let shared = handle.clone();
    let config = config();
    shared
        .replace_server_tools("demo", build(&config, "demo", &[], &demo_offers()))
        .expect("registers");
    assert!(
        handle.current().get("mcp.demo.read_note").is_some(),
        "a clone shares the registry"
    );
    let before = handle.current();
    let mut bad = build(&config, "demo", &[], &[offer("read_note", json!({}))]);
    bad[0].host = quansio_tools::ToolHost::Server;
    assert!(handle.replace_server_tools("demo", bad).is_err());
    assert_eq!(
        handle.current().external().count(),
        before.external().count()
    );
}
