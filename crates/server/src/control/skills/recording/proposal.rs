//! What the plane proposes from a recording, and what the registry may take of it (DOMAIN.md §11.7 D, E).
//!
//! The intelligence plane reads untrusted content and proposes a skill. This module is the server's own
//! check of that proposal, independent of the plane's: the proposal is data in a closed vocabulary, it can
//! name only tools the Tool Registry has and the effect classes those tools need, it holds no screen point
//! and no element index, no unclassified slot becomes part of a skill, and nothing in it matches a
//! redaction rule. A proposal that fails any of this is rejected whole.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use quansio_tools::declaration::EffectClassRule;
use quansio_tools::ToolRegistry;
use regex::Regex;
use serde_json::{json, Map, Value};

use super::config::RecordingConfig;
use super::redact::{field_rule, scan_text, FieldAttrs};
use super::RecordingError;

/// The proposal schema version.
pub const PROPOSAL_SCHEMA_VERSION: &str = "v1";
/// The `procedure` schema version.
pub const PROCEDURE_SCHEMA_VERSION: &str = "v1";

/// Keys of a skill manifest (DOMAIN.md §11.5) plus `procedure` (§11.7 D).
pub const MANIFEST_KEYS: [&str; 8] = [
    "instructions",
    "examples",
    "tool_needs",
    "capability_needs",
    "eval_suite_id",
    "compatibility",
    "recovery_guidance",
    "procedure",
];

const SLOT_TYPES: [&str; 7] = ["text", "number", "choice", "file", "date", "email", "url"];
const PRECONDITION_KINDS: [&str; 4] = ["app_installed", "app_running", "slot_provided", "control_present"];
const POSTCONDITION_KINDS: [&str; 4] = ["app_frontmost", "value_present", "control_present", "evidence_required"];
const UI_ACTIONS: [&str; 8] = [
    "press",
    "set_value",
    "focus",
    "select",
    "open_menu",
    "key",
    "scroll",
    "activate_app",
];
const RECOVERY_ACTIONS: [&str; 3] = ["reobserve_then_ask", "stop_and_ask", "reconcile_by_observation"];

/// Keys that name where something was on the screen, or which one of several it was.
const FORBIDDEN_KEYS: [&str; 19] = [
    "x",
    "y",
    "point",
    "position",
    "coordinate",
    "coordinates",
    "frame",
    "bounds",
    "rect",
    "origin",
    "index",
    "element_index",
    "ref",
    "nth",
    "offset",
    "idx",
    "screen_x",
    "screen_y",
    "pixel",
];

static POINT_TEXT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\(\s*-?\d+(?:\.\d+)?\s*,\s*-?\d+(?:\.\d+)?\s*\)",
        r"(?i)\b[xy]\s*[=:]\s*-?\d+",
        r"\[\s*\d+\s*\]",
        r"(?i)\b(?:index|nth|element)\s*#?\s*\d+\b",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("point pattern"))
    .collect()
});

/// One tool the registry offers, as the compiler is told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSummary {
    /// The tool name.
    pub name: String,
    /// What it does.
    pub description: String,
    /// The effect classes it may settle as or needs a grant of.
    pub effect_classes: Vec<String>,
}

impl ToolSummary {
    /// The wire form the plane reads.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "effect_classes": self.effect_classes,
        })
    }
}

/// Every callable tool of a registry with its effect classes, sorted by name.
#[must_use]
pub fn tool_summaries(registry: &ToolRegistry) -> Vec<ToolSummary> {
    let mut names = registry.callable_names();
    names.sort_unstable();
    names
        .into_iter()
        .filter_map(|name| registry.get(name))
        .map(|declaration| {
            let mut classes = BTreeSet::new();
            match &declaration.effect_class {
                EffectClassRule::Static(class) => {
                    classes.insert(class.as_str().to_string());
                }
                EffectClassRule::Derived(derivation) => {
                    classes.extend(
                        derivation
                            .candidate_effect_classes()
                            .iter()
                            .map(|class| (*class).to_string()),
                    );
                }
            }
            for grant in &declaration.grant_templates {
                classes.insert(grant.effect_class.as_str().to_string());
            }
            ToolSummary {
                name: declaration.name.clone(),
                description: declaration.description.clone(),
                effect_classes: classes.into_iter().collect(),
            }
        })
        .collect()
}

fn reject(rule: &'static str, detail: impl Into<String>) -> RecordingError {
    RecordingError::ProposalRejected {
        rule,
        detail: detail.into(),
    }
}

fn closed<'a>(
    value: &'a Value,
    what: &str,
    keys: &[&str],
    rule: &'static str,
) -> Result<&'a Map<String, Value>, RecordingError> {
    let map = value
        .as_object()
        .ok_or_else(|| reject(rule, format!("{what} must be an object")))?;
    if let Some(unknown) = map.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(reject(rule, format!("{what} has a field outside the closed vocabulary: `{unknown}`")));
    }
    Ok(map)
}

fn list<'a>(map: &'a Map<String, Value>, key: &str, what: &str) -> Result<&'a Vec<Value>, RecordingError> {
    map.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| reject("proposal.procedure", format!("{what}.{key} must be a list")))
}

fn text<'a>(map: &'a Map<String, Value>, key: &str, what: &str) -> Result<&'a str, RecordingError> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| reject("proposal.procedure", format!("{what}.{key} must be non-empty text")))
}

/// A key or a value that says where something was on the screen.
fn find_coordinates(value: &Value, pointer: &str, strings: bool) -> Option<String> {
    match value {
        Value::Object(map) => map.iter().find_map(|(key, inner)| {
            let child = format!("{pointer}/{key}");
            if FORBIDDEN_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
                Some(child)
            } else {
                find_coordinates(inner, &child, strings)
            }
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, inner)| find_coordinates(inner, &format!("{pointer}/{index}"), strings)),
        Value::String(text) if strings => POINT_TEXT
            .iter()
            .any(|pattern| pattern.is_match(text))
            .then(|| pointer.to_string()),
        _ => None,
    }
}

fn find_redactable(value: &Value, pointer: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .iter()
            .find_map(|(key, inner)| find_redactable(inner, &format!("{pointer}/{key}"))),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, inner)| find_redactable(inner, &format!("{pointer}/{index}"))),
        Value::String(text) => (!scan_text(text).is_empty()).then(|| pointer.to_string()),
        _ => None,
    }
}

fn strings_of(map: &Map<String, Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Check a proposal the plane returned for `recording_id`.
///
/// # Errors
/// [`RecordingError::ProposalRejected`] naming the first rule it breaks.
#[allow(clippy::too_many_lines)]
pub fn validate(
    proposal: &Value,
    recording_id: &str,
    tools: &[ToolSummary],
    config: &RecordingConfig,
) -> Result<(), RecordingError> {
    let size = proposal.to_string().len();
    if size > config.proposal_max_bytes {
        return Err(reject("proposal.size", format!("{size} bytes; the limit is {}", config.proposal_max_bytes)));
    }
    let root = closed(
        proposal,
        "the proposal",
        &["schema_version", "trust", "manifest", "slots", "questions"],
        "proposal.shape",
    )?;
    if root.get("schema_version").and_then(Value::as_str) != Some(PROPOSAL_SCHEMA_VERSION) {
        return Err(reject("proposal.shape", "schema_version must be v1"));
    }
    let trust = closed(
        root.get("trust").unwrap_or(&Value::Null),
        "trust",
        &["source", "level", "recording_id", "injection_suspected"],
        "proposal.trust",
    )?;
    if trust.get("source").and_then(Value::as_str) != Some("recording")
        || trust.get("level").and_then(Value::as_str) != Some("UNTRUSTED_EXTERNAL")
        || trust.get("recording_id").and_then(Value::as_str) != Some(recording_id)
    {
        return Err(reject(
            "proposal.trust",
            "a proposal carries its origin: recording, UNTRUSTED_EXTERNAL, and this recording's id",
        ));
    }
    let manifest = closed(
        root.get("manifest").unwrap_or(&Value::Null),
        "the manifest",
        &MANIFEST_KEYS,
        "proposal.manifest_vocabulary",
    )?;
    if manifest.contains_key("eval_suite_id") {
        return Err(reject("proposal.manifest_vocabulary", "a proposal cannot choose its own evaluation suite"));
    }
    if manifest
        .get("instructions")
        .and_then(Value::as_str)
        .is_none_or(|text| text.trim().is_empty())
    {
        return Err(reject("proposal.manifest_vocabulary", "instructions are required"));
    }

    // Tools and effect classes: only what exists, only what those tools need.
    let known: BTreeMap<&str, &ToolSummary> = tools.iter().map(|tool| (tool.name.as_str(), tool)).collect();
    let tool_needs = strings_of(manifest, "tool_needs");
    let mut allowed_classes = BTreeSet::new();
    for name in &tool_needs {
        let Some(tool) = known.get(name.as_str()) else {
            return Err(reject("proposal.tools", format!("`{name}` is not a tool the registry offers")));
        };
        allowed_classes.extend(tool.effect_classes.iter().cloned());
    }
    for class in strings_of(manifest, "capability_needs") {
        if !allowed_classes.contains(&class) {
            return Err(reject(
                "proposal.capabilities",
                format!("`{class}` is not an effect class of a tool the skill needs"),
            ));
        }
    }

    // The procedure.
    let procedure = closed(
        manifest.get("procedure").unwrap_or(&Value::Null),
        "procedure",
        &[
            "schema_version",
            "platform",
            "apps",
            "inputs",
            "preconditions",
            "steps",
            "postconditions",
            "recovery",
        ],
        "proposal.procedure",
    )?;
    if procedure.get("schema_version").and_then(Value::as_str) != Some(PROCEDURE_SCHEMA_VERSION) {
        return Err(reject("proposal.procedure", "procedure.schema_version must be v1"));
    }
    for entry in list(procedure, "apps", "procedure")? {
        let app = closed(entry, "an app", &["bundle_id", "name"], "proposal.procedure")?;
        text(app, "bundle_id", "app")?;
    }
    let slots = root
        .get("slots")
        .and_then(Value::as_array)
        .ok_or_else(|| reject("proposal.slots", "slots must be a list"))?;
    if slots.len() > config.proposal_max_slots {
        return Err(reject("proposal.slots", format!("{} slots; the limit is {}", slots.len(), config.proposal_max_slots)));
    }
    let mut slot_ids = BTreeSet::new();
    for slot in slots {
        let map = closed(
            slot,
            "a slot",
            &[
                "slot_id",
                "name",
                "description",
                "type",
                "example",
                "classification",
                "needs_user_decision",
                "control",
            ],
            "proposal.slots",
        )?;
        let id = map
            .get("slot_id")
            .and_then(Value::as_str)
            .ok_or_else(|| reject("proposal.slots", "a slot needs a slot_id"))?;
        if !slot_ids.insert(id.to_string()) {
            return Err(reject("proposal.slots", format!("slot_id `{id}` is used twice")));
        }
        if !map.get("classification").is_none_or(Value::is_null) {
            return Err(reject(
                "proposal.unclassified",
                "a proposal cannot decide whether a value is a constant or an input; the person does",
            ));
        }
        let kind = map.get("type").and_then(Value::as_str).unwrap_or_default();
        if !SLOT_TYPES.contains(&kind) {
            return Err(reject("proposal.slots", format!("`{kind}` is not a slot type")));
        }
    }
    let mut declared_inputs = BTreeSet::new();
    for entry in list(procedure, "inputs", "procedure")? {
        let input = closed(
            entry,
            "an input",
            &["slot_id", "name", "description", "type", "classification", "constant_value"],
            "proposal.procedure",
        )?;
        let id = text(input, "slot_id", "input")?;
        if !slot_ids.contains(id) {
            return Err(reject("proposal.slots", format!("input `{id}` has no slot")));
        }
        if !input.get("classification").is_none_or(Value::is_null) || input.contains_key("constant_value") {
            return Err(reject("proposal.unclassified", "an input is unclassified until the person decides"));
        }
        declared_inputs.insert(id.to_string());
    }
    if declared_inputs != slot_ids {
        return Err(reject("proposal.slots", "every slot is an input of the procedure, and no input lacks a slot"));
    }
    for entry in list(procedure, "preconditions", "procedure")? {
        let pre = closed(entry, "a precondition", &["kind", "bundle_id", "slot_id", "description"], "proposal.procedure")?;
        let kind = text(pre, "kind", "precondition")?;
        if !PRECONDITION_KINDS.contains(&kind) {
            return Err(reject("proposal.procedure", format!("`{kind}` is not a precondition kind")));
        }
        if let Some(slot) = pre.get("slot_id").and_then(Value::as_str) {
            if !slot_ids.contains(slot) {
                return Err(reject("proposal.slots", format!("precondition names unknown slot `{slot}`")));
            }
        }
    }
    let steps = list(procedure, "steps", "procedure")?;
    if steps.is_empty() || steps.len() > config.proposal_max_steps {
        return Err(reject("proposal.procedure", format!("{} steps; allowed 1..={}", steps.len(), config.proposal_max_steps)));
    }
    let mut step_ids = BTreeSet::new();
    for entry in steps {
        let step = closed(
            entry,
            "a step",
            &["id", "intent", "path", "tools", "slot_ids", "ui", "provenance", "needs_review"],
            "proposal.procedure",
        )?;
        let id = text(step, "id", "step")?;
        if !step_ids.insert(id.to_string()) {
            return Err(reject("proposal.procedure", format!("step id `{id}` is used twice")));
        }
        text(step, "intent", "step")?;
        for slot in strings_of(step, "slot_ids") {
            if !slot_ids.contains(&slot) {
                return Err(reject("proposal.slots", format!("step uses unknown slot `{slot}`")));
            }
        }
        match text(step, "path", "step")? {
            "tool" => {
                let named = strings_of(step, "tools");
                if named.is_empty() || step.contains_key("ui") {
                    return Err(reject("proposal.procedure", "a tool step names tools and no ui"));
                }
                for tool in named {
                    if !tool_needs.contains(&tool) {
                        return Err(reject("proposal.tools", format!("step uses `{tool}` which the skill does not declare it needs")));
                    }
                }
            }
            "ui" => {
                if step.contains_key("tools") {
                    return Err(reject("proposal.procedure", "a ui step names a control, not tools"));
                }
                let ui = closed(
                    step.get("ui").unwrap_or(&Value::Null),
                    "a ui target",
                    &[
                        "bundle_id",
                        "ax_role",
                        "ax_subrole",
                        "ax_title",
                        "ax_identifier",
                        "ax_description",
                        "action",
                        "slot_id",
                        "key",
                    ],
                    "proposal.procedure",
                )?;
                text(ui, "bundle_id", "ui")?;
                let action = text(ui, "action", "ui")?;
                if !UI_ACTIONS.contains(&action) {
                    return Err(reject("proposal.procedure", format!("`{action}` is not a ui action")));
                }
                if let Some(slot) = ui.get("slot_id").and_then(Value::as_str) {
                    if !slot_ids.contains(slot) {
                        return Err(reject("proposal.slots", format!("step uses unknown slot `{slot}`")));
                    }
                }
            }
            other => return Err(reject("proposal.procedure", format!("`{other}` is not a step path"))),
        }
        if let Some(provenance) = step.get("provenance") {
            closed(provenance, "provenance", &["action_seqs"], "proposal.procedure")?;
        }
    }
    for entry in list(procedure, "postconditions", "procedure")? {
        let post = closed(
            entry,
            "a postcondition",
            &["kind", "description", "slot_id", "bundle_id", "ax_role", "ax_title", "ax_identifier"],
            "proposal.procedure",
        )?;
        let kind = text(post, "kind", "postcondition")?;
        if !POSTCONDITION_KINDS.contains(&kind) {
            return Err(reject("proposal.procedure", format!("`{kind}` is not a postcondition kind")));
        }
    }
    let recovery = list(procedure, "recovery", "procedure")?;
    if recovery.is_empty() {
        return Err(reject("proposal.procedure", "a procedure carries recovery"));
    }
    for entry in recovery {
        let item = closed(entry, "a recovery", &["on", "action", "guidance"], "proposal.procedure")?;
        text(item, "on", "recovery")?;
        let action = text(item, "action", "recovery")?;
        if !RECOVERY_ACTIONS.contains(&action) {
            return Err(reject("proposal.procedure", format!("`{action}` is not a recovery action; none repeats an action blind")));
        }
    }

    for entry in root
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| reject("proposal.shape", "questions must be a list"))?
    {
        closed(entry, "a question", &["slot_id", "step_id", "question"], "proposal.shape")?;
    }

    // No coordinates, no element indexes, anywhere in what becomes a skill; nothing redactable anywhere.
    let manifest_value = Value::Object(manifest.clone());
    if let Some(pointer) = find_coordinates(&manifest_value, "/manifest", true) {
        return Err(reject("proposal.no_coordinates", format!("a screen position or element index at {pointer}")));
    }
    for (index, slot) in slots.iter().enumerate() {
        if let Some(pointer) = find_coordinates(slot.get("control").unwrap_or(&Value::Null), &format!("/slots/{index}/control"), false) {
            return Err(reject("proposal.no_coordinates", format!("a screen position or element index at {pointer}")));
        }
    }
    if let Some(pointer) = find_redactable(proposal, "") {
        return Err(reject("proposal.redaction", format!("text a redaction rule removes at {pointer}")));
    }
    Ok(())
}

/// The person's decision for one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotChoice {
    /// The slot.
    pub slot_id: String,
    /// `true` for a constant, `false` for a runtime input.
    pub constant: bool,
}

/// The manifest a submission creates and the classifications it records.
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    /// The manifest of the `DRAFT` version.
    pub manifest: Value,
    /// `[{slot_id, classification}]` for the provenance.
    pub slots: Vec<Value>,
}

/// Apply the person's decisions to a validated proposal.
///
/// # Errors
/// [`RecordingError::Invalid`] when a slot is undecided, a decision names no slot, or a constant would
/// bake in a value that was redacted or entered into a sensitive control.
pub fn build_manifest(proposal: &Value, choices: &[SlotChoice]) -> Result<Built, RecordingError> {
    let slots = proposal["slots"].as_array().cloned().unwrap_or_default();
    let by_id: BTreeMap<&str, &Value> = slots
        .iter()
        .filter_map(|slot| slot["slot_id"].as_str().map(|id| (id, slot)))
        .collect();
    let mut decided = BTreeMap::new();
    for choice in choices {
        if !by_id.contains_key(choice.slot_id.as_str()) {
            return Err(RecordingError::Invalid(format!("`{}` is not a slot of this proposal", choice.slot_id)));
        }
        if decided.insert(choice.slot_id.as_str(), choice.constant).is_some() {
            return Err(RecordingError::Invalid(format!("slot `{}` is decided twice", choice.slot_id)));
        }
    }
    if let Some(missing) = by_id.keys().find(|id| !decided.contains_key(*id)) {
        return Err(RecordingError::Invalid(format!(
            "slot `{missing}` has no decision: say whether it is a constant or an input"
        )));
    }
    let mut manifest = proposal["manifest"].clone();
    let inputs = manifest["procedure"]["inputs"]
        .as_array_mut()
        .ok_or_else(|| RecordingError::Malformed("a stored proposal lost its inputs".to_string()))?;
    let mut summary = Vec::new();
    for input in inputs.iter_mut() {
        let id = input["slot_id"].as_str().unwrap_or_default().to_string();
        let constant = decided[id.as_str()];
        let slot = by_id[id.as_str()];
        let object = input.as_object_mut().expect("an input is an object");
        object.remove("example");
        if constant {
            let value = slot["example"].as_str().unwrap_or_default();
            let control = &slot["control"];
            let attrs = FieldAttrs {
                role: control["ax_role"].as_str(),
                subrole: control["ax_subrole"].as_str(),
                title: control["ax_title"].as_str(),
                identifier: control["ax_identifier"].as_str(),
                description: control["ax_description"].as_str(),
            };
            if value.is_empty() || value.contains("[REDACTED:") || field_rule(&attrs).is_some() {
                return Err(RecordingError::Invalid(format!(
                    "slot `{id}` cannot be a constant: its value was redacted or entered into a sensitive control"
                )));
            }
            object.insert("classification".to_string(), json!("constant"));
            object.insert("constant_value".to_string(), json!(value));
        } else {
            object.insert("classification".to_string(), json!("input"));
        }
        summary.push(json!({
            "slot_id": id,
            "classification": if constant { "constant" } else { "input" },
        }));
    }
    Ok(Built {
        manifest,
        slots: summary,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tools() -> Vec<ToolSummary> {
        tool_summaries(ToolRegistry::builtin())
    }

    pub(crate) fn proposal() -> Value {
        json!({
            "schema_version": "v1",
            "trust": { "source": "recording", "level": "UNTRUSTED_EXTERNAL", "recording_id": "rec_01J8Z3K6F1N8VQ2X5W9YADP003", "injection_suspected": [] },
            "manifest": {
                "instructions": "Follow procedure.",
                "examples": ["make a note"],
                "tool_needs": ["computer.read", "computer.click", "computer.type"],
                "capability_needs": ["read.internal", "computer.input.privileged"],
                "compatibility": ["v8.1"],
                "recovery_guidance": "Re-observe once, then ask the person.",
                "procedure": {
                    "schema_version": "v1",
                    "platform": "macos",
                    "apps": [{ "bundle_id": "com.apple.Notes", "name": "Notes" }],
                    "inputs": [{ "slot_id": "slot_1", "name": "note_body", "description": "Text typed in Note body", "type": "text" }],
                    "preconditions": [{ "kind": "app_installed", "bundle_id": "com.apple.Notes", "description": "Notes is installed" }],
                    "steps": [
                        { "id": "s1", "intent": "Press New Note in Notes", "path": "ui",
                          "ui": { "bundle_id": "com.apple.Notes", "ax_role": "AXButton", "ax_title": "New Note", "action": "press" },
                          "provenance": { "action_seqs": [1] } },
                        { "id": "s2", "intent": "Enter the note body", "path": "ui",
                          "ui": { "bundle_id": "com.apple.Notes", "ax_role": "AXTextArea", "ax_title": "Note body", "action": "set_value", "slot_id": "slot_1" },
                          "provenance": { "action_seqs": [2] } }
                    ],
                    "postconditions": [{ "kind": "value_present", "slot_id": "slot_1", "description": "The note body holds the value" }],
                    "recovery": [{ "on": "any", "action": "reobserve_then_ask", "guidance": "Observe again once; if the control is missing, ask the person." }]
                }
            },
            "slots": [{
                "slot_id": "slot_1", "name": "note_body", "description": "Text typed in Note body", "type": "text",
                "example": "Quarterly report", "classification": null, "needs_user_decision": true,
                "control": { "ax_role": "AXTextArea", "ax_title": "Note body" }
            }],
            "questions": [{ "slot_id": "slot_1", "question": "Is this a constant or an input?" }]
        })
    }

    fn check(proposal: &Value) -> Result<(), RecordingError> {
        validate(proposal, "rec_01J8Z3K6F1N8VQ2X5W9YADP003", &tools(), RecordingConfig::committed())
    }

    fn rule_of(result: Result<(), RecordingError>) -> &'static str {
        match result {
            Err(RecordingError::ProposalRejected { rule, .. }) => rule,
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn a_well_formed_proposal_passes() {
        check(&proposal()).expect("valid");
    }

    #[test]
    fn coordinates_and_element_indexes_are_refused_by_key_and_by_value() {
        for (path, value) in [
            ("/manifest/procedure/steps/0/ui/x", json!(120)),
            ("/manifest/procedure/steps/0/ui/point", json!({ "a": 1 })),
            ("/manifest/procedure/steps/0/ui/element_index", json!(3)),
            ("/manifest/procedure/steps/0/provenance/offset", json!(3)),
            ("/manifest/procedure/steps/0/intent", json!("Click at (120, 340)")),
            ("/manifest/procedure/steps/0/intent", json!("Press AXButton[2]")),
            ("/manifest/procedure/steps/1/intent", json!("press element 4")),
            ("/manifest/procedure/steps/1/intent", json!("go to x=10")),
        ] {
            let mut document = proposal();
            let (parent, key) = path.rsplit_once('/').unwrap();
            let target = document.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            target.insert(key.to_string(), value);
            let rule = rule_of(check(&document));
            // A key outside the closed vocabulary is caught first; both are refusals of the same proposal.
            assert!(
                rule == "proposal.no_coordinates" || rule == "proposal.procedure",
                "{path}: {rule}"
            );
        }
        let mut document = proposal();
        document["manifest"]["procedure"]["steps"][0]["intent"] = json!("Click at (120, 340)");
        assert_eq!(rule_of(check(&document)), "proposal.no_coordinates");
        let mut document = proposal();
        document["slots"][0]["control"]["frame"] = json!({ "w": 10 });
        assert_eq!(rule_of(check(&document)), "proposal.no_coordinates");
    }

    #[test]
    fn a_proposal_cannot_add_a_tool_a_capability_or_an_evaluation() {
        let mut document = proposal();
        document["manifest"]["tool_needs"] = json!(["computer.read", "terminal.exec", "made.up"]);
        assert_eq!(rule_of(check(&document)), "proposal.tools");
        let mut document = proposal();
        document["manifest"]["capability_needs"] = json!(["fs.write.host"]);
        assert_eq!(rule_of(check(&document)), "proposal.capabilities");
        let mut document = proposal();
        document["manifest"]["eval_suite_id"] = json!("eval_coding");
        assert_eq!(rule_of(check(&document)), "proposal.manifest_vocabulary");
        let mut document = proposal();
        document["manifest"]["grants"] = json!(["fs.write.host"]);
        assert_eq!(rule_of(check(&document)), "proposal.manifest_vocabulary");
        let mut document = proposal();
        document["manifest"]["procedure"]["steps"][0]["tools"] = json!(["terminal.exec"]);
        assert_eq!(rule_of(check(&document)), "proposal.procedure");
    }

    #[test]
    fn the_person_decides_every_slot_and_the_proposal_never_does() {
        let mut document = proposal();
        document["slots"][0]["classification"] = json!("constant");
        assert_eq!(rule_of(check(&document)), "proposal.unclassified");
        let mut document = proposal();
        document["manifest"]["procedure"]["inputs"][0]["constant_value"] = json!("Quarterly report");
        assert_eq!(rule_of(check(&document)), "proposal.unclassified");
        let mut document = proposal();
        document["manifest"]["procedure"]["steps"][1]["ui"]["slot_id"] = json!("slot_9");
        assert_eq!(rule_of(check(&document)), "proposal.slots");
    }

    #[test]
    fn nothing_a_redaction_rule_removes_survives_in_a_proposal() {
        for (path, value) in [
            ("/slots/0/example", "AKIAIOSFODNN7EXAMPLE"),
            ("/manifest/instructions", "pay with 4111 1111 1111 1111"),
            ("/questions/0/question", "code 123456"),
        ] {
            let mut document = proposal();
            *document.pointer_mut(path).unwrap() = json!(value);
            assert_eq!(rule_of(check(&document)), "proposal.redaction", "{path}");
        }
    }

    #[test]
    fn the_origin_is_part_of_the_proposal() {
        let edits: [fn(&mut Value); 3] = [
            |d| d["trust"]["level"] = json!("TRUSTED_USER"),
            |d| d["trust"]["source"] = json!("model"),
            |d| d["trust"]["recording_id"] = json!("rec_01J8Z3K6F1N8VQ2X5W9YOTHER1"),
        ];
        for edit in edits {
            let mut document = proposal();
            edit(&mut document);
            assert_eq!(rule_of(check(&document)), "proposal.trust");
        }
    }

    #[test]
    fn a_submission_needs_every_slot_decided_and_never_bakes_in_an_input_example() {
        let proposal = proposal();
        let undecided = build_manifest(&proposal, &[]).expect_err("undecided");
        assert!(matches!(undecided, RecordingError::Invalid(_)), "{undecided:?}");
        let as_input = build_manifest(
            &proposal,
            &[SlotChoice { slot_id: "slot_1".into(), constant: false }],
        )
        .expect("input");
        let rendered = as_input.manifest.to_string();
        assert!(!rendered.contains("Quarterly report"), "an input's example never enters the manifest");
        assert_eq!(as_input.manifest["procedure"]["inputs"][0]["classification"], "input");
        let as_constant = build_manifest(
            &proposal,
            &[SlotChoice { slot_id: "slot_1".into(), constant: true }],
        )
        .expect("constant");
        assert_eq!(as_constant.manifest["procedure"]["inputs"][0]["constant_value"], "Quarterly report");
        assert!(matches!(
            build_manifest(&proposal, &[
                SlotChoice { slot_id: "slot_1".into(), constant: true },
                SlotChoice { slot_id: "slot_1".into(), constant: false },
            ]),
            Err(RecordingError::Invalid(_))
        ));
        assert!(matches!(
            build_manifest(&proposal, &[SlotChoice { slot_id: "nope".into(), constant: false }]),
            Err(RecordingError::Invalid(_))
        ));
    }

    #[test]
    fn a_redacted_or_sensitive_value_cannot_become_a_constant() {
        let mut redacted = proposal();
        redacted["slots"][0]["example"] = json!("[REDACTED:verification_code]");
        assert!(matches!(
            build_manifest(&redacted, &[SlotChoice { slot_id: "slot_1".into(), constant: true }]),
            Err(RecordingError::Invalid(_))
        ));
        let mut sensitive = proposal();
        sensitive["slots"][0]["control"] = json!({ "ax_role": "AXTextField", "ax_title": "Password" });
        assert!(matches!(
            build_manifest(&sensitive, &[SlotChoice { slot_id: "slot_1".into(), constant: true }]),
            Err(RecordingError::Invalid(_))
        ));
        assert!(build_manifest(&sensitive, &[SlotChoice { slot_id: "slot_1".into(), constant: false }]).is_ok());
    }
}
