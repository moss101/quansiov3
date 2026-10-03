//! The managed browser's Tool declarations and their side-effect classes (ADP-001, DOMAIN.md §7.5).
//!
//! Three properties are asserted from the shipped configuration rather than from a fixture:
//!
//! * every `browser.*` Tool declares a side-effect class, and the class its declaration names is the one
//!   `config/effects.yaml` maps it to — a Tool cannot lower its tier by choosing another class;
//! * no mutating browser Tool can be called without naming what it acts on in a form the driver can
//!   check: there is no selector-only click or type, and each requires the document revision;
//! * plain arguments plan: a call derives the privileged-input class, and its idempotency digest moves
//!   with the snapshot, the ref and the revision it was planned against, so a replay of one observation
//!   is one key and a fresh observation is another.

use quansio_capability::{EffectClass, Tier};
use quansio_tools::call::CallContext;
use quansio_tools::{
    plan_call, SideEffectClass, SideEffectMapping, SourceTrust, ToolHost, ToolRegistry,
};
use serde_json::{json, Value};

fn tier_of(class: &EffectClass) -> Option<Tier> {
    let tier = match class.as_str() {
        "read.internal" | "read.external" => 0,
        "network.egress.new_destination" => 2,
        "computer.input.privileged" => 3,
        _ => return None,
    };
    Tier::new(tier).ok()
}

fn context() -> CallContext<'static> {
    CallContext {
        workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        run_id: Some("run_01J8Z3K6F1N8VQ2X5W9Y0FFFFF"),
        fs_root: None,
        own_branch: None,
    }
}

#[test]
fn every_browser_tool_declares_a_side_effect_class_that_agrees_with_the_mapping() {
    let registry = ToolRegistry::builtin();
    let mapping = SideEffectMapping::builtin();
    let browser_tools: Vec<_> = registry
        .declarations()
        .filter(|declaration| declaration.name.starts_with("browser."))
        .collect();
    assert!(browser_tools.len() >= 8, "{}", browser_tools.len());
    for declaration in &browser_tools {
        let class = declaration
            .side_effect
            .unwrap_or_else(|| panic!("{} declares no side_effect", declaration.name));
        let row = mapping.row(class);
        assert!(
            matches!(&declaration.effect_class, quansio_tools::EffectClassRule::Static(declared) if declared == &row.effect_class),
            "{}: side_effect {} maps to {}",
            declaration.name,
            class.as_str(),
            row.effect_class
        );
        assert_eq!(declaration.host, ToolHost::Browser, "{}", declaration.name);
        assert_eq!(
            declaration.source_trust,
            SourceTrust::UntrustedExternal,
            "{} does not label its results untrusted",
            declaration.name
        );
    }

    // Which tools are reads and which are mutations is the part a policy reviewer wants to see.
    let class_of = |name: &str| registry.get(name).and_then(|d| d.side_effect).expect(name);
    assert_eq!(class_of("browser.observe"), SideEffectClass::Read);
    assert_eq!(class_of("browser.extract"), SideEffectClass::Read);
    assert_eq!(
        class_of("browser.screenshot"),
        SideEffectClass::ReadSensitive
    );
    assert_eq!(
        class_of("browser.navigate"),
        SideEffectClass::OpenWorldMutation
    );
    assert_eq!(class_of("browser.type"), SideEffectClass::LocalMutation);
    assert_eq!(
        class_of("browser.press_key"),
        SideEffectClass::LocalMutation
    );
    assert_eq!(
        class_of("browser.handle_dialog"),
        SideEffectClass::LocalMutation
    );
    assert_eq!(
        class_of("browser.click"),
        SideEffectClass::PotentialExternalEffect
    );
    // Reads are never journaled; every mutation is.
    for declaration in &browser_tools {
        let class = declaration.side_effect.expect("class");
        let mutating = !matches!(
            class,
            SideEffectClass::Read | SideEffectClass::ReadSensitive
        );
        assert_eq!(
            mapping.row(class).journaled,
            mutating,
            "{}",
            declaration.name
        );
    }
}

#[test]
fn a_browser_tool_that_disagrees_with_its_class_or_declares_none_is_refused() {
    let mapping = SideEffectMapping::builtin();
    let shipped = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/tools.yaml"
    ))
    .expect("read config/tools.yaml");

    // A click that claims to be a read: its side_effect says mutation, its class (and its grant) say
    // read. The registry itself is consistent, so only the side-effect mapping can refuse it.
    let start = shipped
        .find("  - name: browser.click\n")
        .expect("browser.click is declared");
    let end = start
        + shipped[start + 4..]
            .find("\n  - name: ")
            .map_or(shipped.len() - start - 4, |offset| offset + 4);
    let block = shipped[start..end].replace("computer.input.privileged", "read.external");
    let lowered = format!("{}{}{}", &shipped[..start], block, &shipped[end..]);
    assert_ne!(lowered, shipped, "the replacement did not apply");
    let registry = ToolRegistry::from_yaml(&lowered).expect("still a valid registry");
    let refusal = registry
        .check_side_effects(mapping)
        .expect_err("the lowered tier is refused");
    assert!(refusal.to_string().contains("browser.click"), "{refusal}");

    // A browser tool with no side-effect class at all.
    let silent = shipped.replacen("    side_effect: open_world_mutation\n", "", 1);
    assert_ne!(silent, shipped);
    let registry = ToolRegistry::from_yaml(&silent).expect("still a valid registry");
    let refusal = registry
        .check_side_effects(mapping)
        .expect_err("a missing class is refused");
    assert!(
        refusal.to_string().contains("browser.navigate"),
        "{refusal}"
    );

    // An unknown class is not one of the five.
    let unknown = shipped.replacen("side_effect: read\n", "side_effect: harmless\n", 1);
    assert!(ToolRegistry::from_yaml(&unknown).is_err());
}

#[test]
fn there_is_no_selector_only_mutating_browser_tool() {
    let registry = ToolRegistry::builtin();
    for name in [
        "browser.click",
        "browser.type",
        "browser.press_key",
        "browser.handle_dialog",
    ] {
        let declaration = registry.get(name).expect(name);
        let schema = &declaration.input_schema;
        let properties = schema["properties"].as_object().expect("properties");
        assert!(
            !properties.contains_key("selector"),
            "{name} can be called with a bare selector"
        );
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            required.contains(&"expected_document_revision"),
            "{name} does not require a revision"
        );
        assert!(
            required.contains(&"tab_id"),
            "{name} does not require a tab"
        );
    }
    // The two that act on an element name it as a ref or as a point, and nothing else.
    for name in ["browser.click", "browser.type"] {
        let properties = registry.get(name).expect(name).input_schema["properties"].clone();
        for field in ["snapshot_id", "ref", "capture_id", "x", "y"] {
            assert!(
                properties.get(field).is_some(),
                "{name} cannot name {field}"
            );
        }
    }
    // And no browser tool accepts a certificate or a permission decision: none is named for one, and
    // none has an argument that could carry one (`accept` answers a dialog, which is not one).
    for name in registry
        .callable_names()
        .into_iter()
        .filter(|name| name.starts_with("browser."))
    {
        for forbidden in [
            "certificate",
            "permission",
            "grant",
            "accept",
            "allow",
            "continue",
        ] {
            assert!(!name.contains(forbidden), "{name}");
        }
        let properties = registry.get(name).expect(name).input_schema["properties"].clone();
        for key in properties.as_object().expect("properties").keys() {
            for forbidden in ["certificate", "permission", "grant", "allow", "continue"] {
                assert!(!key.contains(forbidden), "{name} accepts {key}");
            }
        }
    }
}

#[test]
fn a_call_plans_as_privileged_input_and_its_key_moves_with_what_it_was_planned_against() {
    let registry = ToolRegistry::builtin();
    let click = |snapshot: &str, reference: &str, revision: &str| {
        plan_call(
            registry,
            "browser.click",
            None,
            &json!({
                "session_id": "bsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
                "tab_id": "TAB",
                "expected_document_revision": revision,
                "snapshot_id": snapshot,
                "ref": reference,
            }),
            &tier_of,
            &context(),
        )
        .expect("a valid click plans")
    };
    let first = click("snap_a", "e1", "rev_1");
    assert_eq!(first.effect_class.as_str(), "computer.input.privileged");
    assert_eq!(first.tier.get(), 3);
    assert_eq!(first.host, ToolHost::Browser);
    assert_eq!(first.source_trust, SourceTrust::UntrustedExternal);
    // The same observation is the same key; another observation, ref or revision is another.
    assert_eq!(
        first.idempotency_key.canonical(),
        click("snap_a", "e1", "rev_1").idempotency_key.canonical()
    );
    for other in [
        click("snap_b", "e1", "rev_1"),
        click("snap_a", "e2", "rev_1"),
        click("snap_a", "e1", "rev_2"),
    ] {
        assert_ne!(
            first.idempotency_key.canonical(),
            other.idempotency_key.canonical()
        );
        assert_ne!(first.params_digest, other.params_digest);
    }

    // A selector is not a field of any browser mutation: it is refused as an unknown argument.
    let refused = plan_call(
        registry,
        "browser.click",
        None,
        &json!({
            "session_id": "bsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
            "tab_id": "TAB",
            "expected_document_revision": "rev_1",
            "selector": "#buy",
        }),
        &tier_of,
        &context(),
    );
    assert!(refused.is_err());

    // Typing keeps the placeholder form in its digest material: the call's arguments are the model's.
    let typed = plan_call(
        registry,
        "browser.type",
        None,
        &json!({
            "session_id": "bsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
            "tab_id": "TAB",
            "expected_document_revision": "rev_1",
            "snapshot_id": "snap_a",
            "ref": "e3",
            "text": "{{secret:acme.password}}",
        }),
        &tier_of,
        &context(),
    )
    .expect("a placeholder plans");
    assert_eq!(typed.redact, vec!["text".to_string()]);
    assert_eq!(typed.args["text"], "{{secret:acme.password}}");
}
