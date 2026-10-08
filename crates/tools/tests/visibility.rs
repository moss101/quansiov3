//! Tool visibility (ADP-008, DOMAIN.md §7.5): who may propose a tool is declared, and the split only narrows.
//!
//! * `model_visible` / `host_callable` parse, default and fail closed (an unknown key, a non-boolean, a tool nobody can
//!   call are all refused);
//! * a host-only tool is absent from every model-facing view of the registry whatever the projection holds, a
//!   projection can only remove tools from that view (the narrowing property, over every subset of a grant pool), and an
//!   older version of a tool never makes a name visible;
//! * a model that names a host-only tool is refused before its arguments are read, with a typed code; the host path
//!   reaches it; a tool the host may not call is refused to the host;
//! * a source scan proves no crate but this one reads the unfiltered declaration list, so a new model-facing surface
//!   (a tool search, an MCP or SDK export) has to come through the filter.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use quansio_capability::projection::CapabilityProjection;
use quansio_capability::{EffectClass, Grant, SubjectKind, Tier};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_tools::call::{CallContext, Caller};
use quansio_tools::{plan_call, ToolError, ToolRegistry};
use serde_json::json;

fn tier_of(_class: &EffectClass) -> Option<Tier> {
    Tier::new(0).ok()
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

fn context(caller: Caller) -> CallContext<'static> {
    CallContext {
        caller,
        workspace_id: "ws_test",
        run_id: Some("run_test"),
        fs_root: Some("/work/root"),
        own_branch: None,
    }
}

/// One read-only tool, with the visibility lines the test supplies.
fn tool(name: &str, version: u32, visibility: &str) -> String {
    format!(
        r#"
  - name: {name}
    version: {version}
    description: A fixture tool.
    host: server
    source_trust: untrusted_external
    effect_class: read.internal
    resource: {{ kind: fs, selector: "**" }}
    idempotency: {{ digest_args: [key] }}
    max_output_bytes: 1024
    timeout_ms: 1000
    cancellable: false
    evidence: {{ kinds: [], redact: [] }}
    grant_templates:
      - effect_class: read.internal
        resource: {{ kind: fs, selector: "**" }}
        constraints: {{}}
    {visibility}
    input_schema:
      type: object
      additionalProperties: false
      properties:
        key: {{ type: string }}
      required: [key]
    output_schema:
      type: object
      additionalProperties: false
      properties:
        ok: {{ type: boolean }}
      required: [ok]
"#
    )
}

fn registry(tools: &[String]) -> Result<ToolRegistry, ToolError> {
    ToolRegistry::from_yaml(&format!("version: 1\ntools:{}", tools.concat()))
}

fn fixture() -> ToolRegistry {
    registry(&[
        tool("note.read", 1, ""),
        tool(
            "panel.list",
            1,
            "model_visible: false\n    host_callable: true",
        ),
        tool("panel.refresh", 1, "host_callable: true"),
    ])
    .expect("fixture registry")
}

// ---- declaration ------------------------------------------------------------------------------

#[test]
fn a_declaration_is_model_visible_and_not_host_callable_unless_it_says_otherwise() {
    let registry = fixture();
    let plain = registry.get("note.read").expect("note.read");
    assert!(plain.model_visible && !plain.host_callable);
    let host_only = registry.get("panel.list").expect("panel.list");
    assert!(!host_only.model_visible && host_only.host_callable);
    let both = registry.get("panel.refresh").expect("panel.refresh");
    assert!(both.model_visible && both.host_callable);
}

#[test]
fn a_tool_nobody_can_call_is_refused() {
    let error = registry(&[tool(
        "dead.end",
        1,
        "model_visible: false\n    host_callable: false",
    )])
    .expect_err("neither visible nor callable");
    assert!(
        matches!(error, ToolError::MalformedDeclaration { ref tool, .. } if tool == "dead.end"),
        "{error:?}"
    );
}

#[test]
fn a_misspelt_or_ill_typed_flag_cannot_leave_a_tool_visible() {
    // A typo would otherwise be ignored and leave the default (visible) in force.
    let misspelt = registry(&[tool(
        "note.read",
        1,
        "model_visble: false\n    host_callable: true",
    )])
    .expect_err("an unknown key is refused");
    assert!(matches!(misspelt, ToolError::Config(_)), "{misspelt:?}");
    let ill_typed = registry(&[tool("note.read", 1, "model_visible: \"no\"")])
        .expect_err("a string is not a boolean");
    assert!(matches!(ill_typed, ToolError::Config(_)), "{ill_typed:?}");
}

// ---- the shipped registry ---------------------------------------------------------------------

/// The host-only tools: `webdev.preview_scopes` (ADP-008) and `browser.import_session` (ADP-009, a person's staged cookies, applied only
/// after an approval that is asked every time).
const HOST_ONLY: [&str; 2] = ["browser.import_session", "webdev.preview_scopes"];

#[test]
fn the_shipped_registry_declares_exactly_two_host_only_tools_and_the_rest_are_the_models() {
    let registry = ToolRegistry::builtin();
    let mut host_only: Vec<&str> = registry
        .declarations()
        .filter(|declaration| !declaration.model_visible)
        .map(|declaration| declaration.name.as_str())
        .collect();
    host_only.sort_unstable();
    assert_eq!(host_only, HOST_ONLY);
    let mut host_callable = registry.host_callable_names();
    host_callable.sort_unstable();
    assert_eq!(host_callable, HOST_ONLY);
    let all: BTreeSet<&str> = registry.callable_names().into_iter().collect();
    let visible: BTreeSet<&str> = registry.model_visible_names().into_iter().collect();
    assert_eq!(all.len(), visible.len() + HOST_ONLY.len());
    for name in HOST_ONLY {
        assert!(!visible.contains(name), "{name} is offered to a model");
    }
    // The audit: every other tool keeps the defaults.
    for declaration in registry.declarations() {
        if !HOST_ONLY.contains(&declaration.name.as_str()) {
            assert!(
                declaration.model_visible && !declaration.host_callable,
                "{} changed visibility",
                declaration.name
            );
        }
    }
}

// ---- host-only absence and narrowing ----------------------------------------------------------

#[test]
fn a_host_only_tool_is_absent_from_every_model_facing_view_whatever_the_projection_holds() {
    let registry = ToolRegistry::builtin();
    let now = Utc::now();
    // The widest projection there is: a grant of every class the registry's tools are exposed by, over everything.
    let mut classes: BTreeSet<String> = BTreeSet::new();
    for declaration in registry.declarations() {
        for template in &declaration.grant_templates {
            classes.insert(template.effect_class.as_str().to_string());
        }
    }
    let widest = project(
        classes
            .iter()
            .map(|class| grant(class, "fs", "**"))
            .chain(classes.iter().map(|class| grant(class, "domain", "*")))
            .collect(),
    );
    let host_tool = registry
        .get("webdev.preview_scopes")
        .expect("the host-only tool is declared");
    assert!(
        host_tool.covered_by(&widest.grants, now),
        "the grants do cover it, so only visibility keeps it out"
    );
    assert!(!registry.is_exposed("webdev.preview_scopes", &widest, now));
    assert!(!registry
        .exposed_names(&widest, now)
        .contains(&"webdev.preview_scopes"));
    assert!(registry
        .exposed_to(&widest, now)
        .iter()
        .all(|declaration| declaration.name != "webdev.preview_scopes"));
    assert!(!registry
        .model_visible_names()
        .contains(&"webdev.preview_scopes"));
    // ...and the widest projection still exposes the model's own read tools: the filter removed one tool, not the surface.
    assert!(registry.is_exposed("fs.read", &widest, now));
    assert!(registry.is_exposed("webdev.preview_open", &widest, now));
}

#[test]
fn a_projection_can_only_remove_tools_from_the_model_view() {
    let registry = fixture();
    let now = Utc::now();
    let pool = [
        grant("read.internal", "fs", "**"),
        grant("read.internal", "fs", "docs/**"),
        grant("read.external", "domain", "*"),
        grant("record.create", "work", "**"),
    ];
    let ceiling: BTreeSet<&str> = registry.model_visible_names().into_iter().collect();
    // Every subset of the grant pool: exposure of a subset is inside exposure of any superset, and inside the ceiling.
    let exposure = |mask: usize| -> BTreeSet<String> {
        let grants: Vec<Grant> = pool
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, grant)| grant.clone())
            .collect();
        registry
            .exposed_names(&project(grants), now)
            .into_iter()
            .map(str::to_string)
            .collect()
    };
    for narrow in 0..(1_usize << pool.len()) {
        let narrow_set = exposure(narrow);
        assert!(
            narrow_set
                .iter()
                .all(|name| ceiling.contains(name.as_str())),
            "{narrow:b} exposes {narrow_set:?} beyond the ceiling {ceiling:?}"
        );
        assert!(
            !narrow_set.contains("panel.list"),
            "host-only leaked at {narrow:b}"
        );
        for wide in 0..(1_usize << pool.len()) {
            if narrow & wide == narrow {
                assert!(
                    narrow_set.is_subset(&exposure(wide)),
                    "adding grants removed or reordered exposure: {narrow:b} -> {wide:b}"
                );
            }
        }
    }
    // The ceiling is reached by the full pool and is never exceeded: visibility is not something a grant adds.
    assert_eq!(
        exposure((1 << pool.len()) - 1),
        BTreeSet::from(["note.read".to_string(), "panel.refresh".to_string()])
    );
}

#[test]
fn an_older_version_never_makes_a_name_visible() {
    let registry = registry(&[
        tool("panel.list", 1, ""),
        tool(
            "panel.list",
            2,
            "model_visible: false\n    host_callable: true",
        ),
    ])
    .expect("registry");
    let projection = project(vec![grant("read.internal", "fs", "**")]);
    let now = Utc::now();
    assert!(registry.model_visible_names().is_empty());
    assert!(registry.exposed_names(&projection, now).is_empty());
    assert!(!registry.is_exposed("panel.list", &projection, now));
    assert_eq!(registry.host_callable_names(), vec!["panel.list"]);
}

// ---- who may call -----------------------------------------------------------------------------

#[test]
fn a_model_naming_a_host_only_tool_is_refused_before_its_arguments_are_read() {
    let registry = fixture();
    for args in [json!({ "key": "k" }), json!({}), json!({ "bogus": 1 })] {
        let error = plan_call(
            &registry,
            "panel.list",
            None,
            &args,
            &tier_of,
            &context(Caller::Model),
        )
        .expect_err("a model may not call a host-only tool");
        assert!(
            matches!(error, ToolError::NotModelVisible { ref tool } if tool == "panel.list"),
            "{error:?}"
        );
        assert_eq!(error.code(), "TOOL_NOT_MODEL_VISIBLE");
    }
    // A pinned version does not get around it.
    let pinned = plan_call(
        &registry,
        "panel.list",
        Some(1),
        &json!({ "key": "k" }),
        &tier_of,
        &context(Caller::Model),
    )
    .expect_err("pinned");
    assert_eq!(pinned.code(), "TOOL_NOT_MODEL_VISIBLE");
    // The default caller is the model, so a context that never says who it is cannot reach a host-only tool.
    let unnamed = CallContext::default();
    assert_eq!(unnamed.caller, Caller::Model);
    let error = plan_call(
        &registry,
        "panel.list",
        None,
        &json!({ "key": "k" }),
        &tier_of,
        &unnamed,
    )
    .expect_err("default caller");
    assert_eq!(error.code(), "TOOL_NOT_MODEL_VISIBLE");
}

#[test]
fn the_host_reaches_a_host_callable_tool_and_gets_the_same_plan() {
    let registry = fixture();
    let args = json!({ "key": "k" });
    let host = plan_call(
        &registry,
        "panel.list",
        None,
        &args,
        &tier_of,
        &context(Caller::Host),
    )
    .expect("the host may call it");
    assert_eq!(host.tool, "panel.list");
    assert_eq!(host.effect_class.as_str(), "read.internal");
    // A tool that is both is planned identically for either caller: only who asked differs.
    let by_model = plan_call(
        &registry,
        "panel.refresh",
        None,
        &args,
        &tier_of,
        &context(Caller::Model),
    )
    .expect("model");
    let by_host = plan_call(
        &registry,
        "panel.refresh",
        None,
        &args,
        &tier_of,
        &context(Caller::Host),
    )
    .expect("host");
    assert_eq!(by_model, by_host);
    // The host is still validated strictly.
    let invalid = plan_call(
        &registry,
        "panel.list",
        None,
        &json!({}),
        &tier_of,
        &context(Caller::Host),
    )
    .expect_err("arguments are still checked for the host");
    assert_eq!(invalid.code(), "VALIDATION_SCHEMA");
}

#[test]
fn the_host_may_not_call_a_tool_that_is_not_host_callable() {
    let registry = fixture();
    let error = plan_call(
        &registry,
        "note.read",
        None,
        &json!({ "key": "k" }),
        &tier_of,
        &context(Caller::Host),
    )
    .expect_err("a model-only tool is not a host control");
    assert!(
        matches!(error, ToolError::NotHostCallable { ref tool } if tool == "note.read"),
        "{error:?}"
    );
    assert_eq!(error.code(), "TOOL_NOT_HOST_CALLABLE");
}

#[test]
fn the_shipped_host_only_tool_plans_for_the_host_and_not_for_a_model() {
    let registry = ToolRegistry::builtin();
    let args = json!({});
    let model = plan_call(
        registry,
        "webdev.preview_scopes",
        None,
        &args,
        &tier_of,
        &context(Caller::Model),
    )
    .expect_err("model");
    assert_eq!(model.code(), "TOOL_NOT_MODEL_VISIBLE");
    let host = plan_call(
        registry,
        "webdev.preview_scopes",
        None,
        &args,
        &tier_of,
        &context(Caller::Host),
    )
    .expect("host");
    assert_eq!(host.effect_class.as_str(), "read.internal");
}

// ---- no other crate reads the unfiltered list -------------------------------------------------

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_crate_but_the_registry_reads_the_unfiltered_declaration_list() {
    // `declarations()` and `callable_names()` list every declaration, host-only ones included. They exist for the
    // catalog and contract checks; every model-facing list is built from `exposed_*` or `model_visible_names`.
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&crates).expect("crates dir").flatten() {
        if entry.file_name() == "tools" {
            continue;
        }
        let mut files = Vec::new();
        rust_sources(&entry.path().join("src"), &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).expect("source");
            for needle in [".declarations()", ".callable_names()"] {
                if text.contains(needle) {
                    offenders.push(format!("{}: {needle}", file.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a model-facing list must use the filtered views: {offenders:#?}"
    );
}

#[test]
fn the_python_gateway_reads_the_registry_only_for_model_visible_declarations() {
    // The gateway's schema source is the one Python reader of config/tools.yaml; it skips what is not model-visible.
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../python/intelligence/model_gateway/tooling.py"),
    )
    .expect("tooling.py");
    assert!(text.contains("model_visible"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/intelligence");
    let mut readers = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "py") {
                let source = std::fs::read_to_string(&path).unwrap_or_default();
                if source.contains("safe_load") && source.contains("tools.yaml") {
                    readers.push(path);
                }
            }
        }
    }
    assert_eq!(
        readers.len(),
        1,
        "exactly one Python module parses config/tools.yaml: {readers:?}"
    );
    assert!(readers[0].ends_with("model_gateway/tooling.py"));
}
