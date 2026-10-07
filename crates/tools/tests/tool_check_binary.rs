//! `quansio-tool-check` answers with the registry's own argument check (QA-004 seam).
//!
//! The binary is how the evaluation campaign asks the Tool Registry whether a proposed call
//! is valid, so it must agree with `plan_call` exactly: an undeclared tool, an unknown
//! version, a missing required field, an unknown field and a wrong type are all refused, a
//! valid call is accepted, and one verdict comes back per proposal in input order.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

fn check(lines: &[&str]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_quansio-tool-check"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the binary starts");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for line in lines {
            writeln!(stdin, "{line}").expect("write a proposal");
        }
    }
    let output = child.wait_with_output().expect("the binary exits");
    assert!(output.status.success(), "exit status {:?}", output.status);
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON verdict per line"))
        .collect()
}

#[test]
fn verdicts_follow_the_registry_schema_check() {
    let verdicts = check(&[
        r#"{"tool": "fs.read", "args": {"path": "README.md"}}"#,
        r#"{"tool": "fs.read", "args": {}}"#,
        r#"{"tool": "fs.read", "args": {"path": "README.md", "mode": "raw"}}"#,
        r#"{"tool": "fs.read", "args": {"path": 42}}"#,
        r#"{"tool": "fs.read", "args": {"path": ""}}"#,
        r#"{"tool": "fs.teleport", "args": {}}"#,
        r#"{"tool": "fs.read", "version": 99, "args": {"path": "README.md"}}"#,
        r#"{"tool": "fs.read", "version": 1, "args": {"path": "README.md"}}"#,
        "not json",
        r#"{"args": {}}"#,
    ]);
    let summary: Vec<(bool, &str)> = verdicts
        .iter()
        .map(|verdict| {
            (
                verdict["valid"].as_bool().expect("valid is a boolean"),
                verdict["rule"].as_str().expect("rule is a string"),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (true, "ok"),
            (false, "invalid_arguments"),
            (false, "invalid_arguments"),
            (false, "invalid_arguments"),
            (false, "invalid_arguments"),
            (false, "unknown_tool"),
            (false, "unknown_tool_version"),
            (true, "ok"),
            (false, "malformed_proposal"),
            (false, "malformed_proposal"),
        ]
    );
    assert_eq!(
        verdicts[1]["path"], ".path",
        "the missing required field is named"
    );
    assert_eq!(
        verdicts[2]["path"], ".mode",
        "the undeclared field is named"
    );
    assert_eq!(verdicts[3]["path"], ".path");
}

#[test]
fn blank_lines_are_skipped_and_order_is_kept() {
    let verdicts = check(&[
        r#"{"tool": "web.search", "args": {}}"#,
        "",
        r#"{"tool": "fs.read", "args": {"path": "a"}}"#,
    ]);
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0]["tool"], "web.search");
    assert_eq!(verdicts[1]["tool"], "fs.read");
    assert_eq!(verdicts[1]["valid"], true);
}

#[test]
fn a_host_only_tool_is_not_a_tool_a_model_may_propose() {
    // The campaign's proposer is a model (ADP-008): a tool that is not model-visible is refused before its
    // arguments are read, exactly as `plan_call` refuses it for a model.
    let verdicts = check(&[
        r#"{"tool": "webdev.preview_scopes", "args": {}}"#,
        r#"{"tool": "webdev.preview_scopes", "args": {"bogus": 1}}"#,
        r#"{"tool": "webdev.preview_open", "args": {"directory": "."}}"#,
    ]);
    let rules: Vec<&str> = verdicts
        .iter()
        .map(|verdict| verdict["rule"].as_str().expect("rule"))
        .collect();
    assert_eq!(rules, vec!["not_model_visible", "not_model_visible", "ok"]);
    assert_eq!(verdicts[0]["valid"], false);
}
