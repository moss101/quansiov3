//! `quansio-tool-check`: the Tool Registry's own argument check, one proposal per line.
//!
//! Evaluation (QA-004) measures how many proposed tool calls are valid against their
//! declarations. That judgement is this crate's (DOMAIN.md §7.4): a second implementation in
//! another language would be a second, drifting authority. This binary exposes exactly the
//! check `plan_call` applies before dispatch — the tool must be declared (at the requested
//! version, when one is named) and its arguments must pass [`first_violation`] against the
//! declared input schema — and nothing else. It reads, derives nothing and writes no state.
//!
//! Input (stdin): one JSON object per line, `{"tool": "fs.read", "version": 1?, "args": {...}}`.
//! Output (stdout): one JSON object per input line, in order,
//! `{"tool", "valid", "rule", "path"?, "detail"?}` where `rule` is `ok`, `unknown_tool`,
//! `unknown_tool_version`, `invalid_arguments` or `malformed_proposal`.

use std::io::{self, BufRead, Write};

use quansio_tools::{first_violation, ToolRegistry};
use serde_json::{json, Value};

fn verdict(registry: &ToolRegistry, line: &str) -> Value {
    let Ok(Value::Object(proposal)) = serde_json::from_str::<Value>(line) else {
        return json!({"tool": "", "valid": false, "rule": "malformed_proposal",
                      "detail": "a proposal is one JSON object per line"});
    };
    let Some(tool) = proposal.get("tool").and_then(Value::as_str) else {
        return json!({"tool": "", "valid": false, "rule": "malformed_proposal",
                      "detail": "a proposal names its `tool`"});
    };
    let args = proposal.get("args").cloned().unwrap_or(Value::Null);
    let declaration = match proposal.get("version").and_then(Value::as_u64) {
        Some(version) => match u32::try_from(version)
            .ok()
            .and_then(|version| registry.get_version(tool, version))
        {
            Some(declaration) => declaration,
            None => {
                return json!({"tool": tool, "valid": false, "rule": "unknown_tool_version",
                              "detail": format!("{tool} v{version} is not registered")})
            }
        },
        None => match registry.get(tool) {
            Some(declaration) => declaration,
            None => {
                return json!({"tool": tool, "valid": false, "rule": "unknown_tool",
                              "detail": format!("{tool} is not a registered tool")})
            }
        },
    };
    match first_violation(&declaration.input_schema, &args) {
        None => json!({"tool": tool, "valid": true, "rule": "ok"}),
        Some(violation) => json!({"tool": tool, "valid": false, "rule": "invalid_arguments",
                                  "path": violation.path, "detail": violation.detail}),
    }
}

fn main() -> io::Result<()> {
    let registry = ToolRegistry::builtin();
    let stdin = io::stdin();
    let mut out = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        writeln!(out, "{}", verdict(registry, &line))?;
    }
    out.flush()
}
