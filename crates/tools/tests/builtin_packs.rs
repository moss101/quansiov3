//! CAP-007: the built-in packs can run in this runtime.
//!
//! * every tool a built-in skill or capability pack names is callable in the Tool Registry the
//!   runtime dispatches through (`ToolRegistry::builtin`, from `config/tools.yaml`) — not a
//!   reserved family, not a name only the pack knows;
//! * a skill's `capability_needs` are exactly the grant templates its tools require: every tool
//!   is covered by one of its registered templates, and no need is surplus to its tools, so a
//!   skill neither asks for more than it uses nor names a capability that is not an effect class.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use quansio_tools::ToolRegistry;

fn packs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs")
}

fn yaml(path: &Path) -> serde_yaml::Value {
    serde_yaml::from_str(&std::fs::read_to_string(path).expect("read")).expect("yaml")
}

fn strings(value: &serde_yaml::Value) -> Vec<String> {
    value
        .as_sequence()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `(pack, tool names)` for every skill and capability pack.
fn declared_tools() -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for kind in ["skills", "capabilities"] {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(packs().join(kind))
            .expect("packs")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string();
            let tools = if kind == "skills" {
                let path = dir.join("skill.yaml");
                if !path.is_file() {
                    continue;
                }
                strings(&yaml(&path)["tool_needs"])
            } else {
                let path = dir.join("pack.yaml");
                if !path.is_file() {
                    continue;
                }
                strings(&yaml(&path)["contents"]["tool_names"])
            };
            out.push((format!("{kind}/{name}"), tools));
        }
    }
    out
}

#[test]
fn every_tool_a_builtin_pack_names_is_callable_in_the_registry() {
    let registry = ToolRegistry::builtin();
    let callable: BTreeSet<&str> = registry.callable_names().into_iter().collect();
    let declared = declared_tools();
    assert!(
        declared.len() >= 12,
        "six day-one domains, as skills and as capability packs"
    );
    for (pack, tools) in &declared {
        assert!(!tools.is_empty(), "{pack} names no tool");
        for tool in tools {
            assert!(
                callable.contains(tool.as_str()),
                "{pack} needs {tool}, which the runtime's Tool Registry cannot dispatch"
            );
        }
    }
}

#[test]
fn a_skills_capability_needs_are_exactly_what_its_tools_require() {
    let registry = ToolRegistry::builtin();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(packs().join("skills"))
        .expect("skills")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("skill.yaml").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let manifest = yaml(&dir.join("skill.yaml"));
        let needs: BTreeSet<String> = strings(&manifest["capability_needs"]).into_iter().collect();
        let mut justified = BTreeSet::new();
        for tool in strings(&manifest["tool_needs"]) {
            let declaration = registry
                .get(&tool)
                .expect("callable (see the availability test)");
            let templates: BTreeSet<String> = declaration
                .grant_templates
                .iter()
                .map(|grant| grant.effect_class.as_str().to_string())
                .collect();
            let covering: Vec<&String> = templates.intersection(&needs).collect();
            assert!(
                !covering.is_empty(),
                "{}: {tool} needs one of {templates:?}, the skill declares {needs:?}",
                dir.display()
            );
            justified.extend(covering.into_iter().cloned());
        }
        let surplus: Vec<&String> = needs.difference(&justified).collect();
        assert!(
            surplus.is_empty(),
            "{}: {surplus:?} is not required by any of its tools",
            dir.display()
        );
    }
}
