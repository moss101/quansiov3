//! The plane's half of an install, as the runtime asks for it (ADP-008).
//!
//! [`PackCompilePort`] is how the installer gets a pack verified and compiled. The deployable's port is
//! [`GatewayPackCompiler`], a `CompilePack` call to the intelligence plane (`python/intelligence/capability_compiler`).
//! What comes back is a proposal about the pack, read strictly: a phase, a severity or a rule the runtime does not
//! know is a refusal of the answer, and the runtime decides what a finding means for the install.

use std::time::Duration;

use async_trait::async_trait;
use quansio_tools::ToolVisibility;
use serde_json::{json, Value};

use super::config::InstallConfig;
use super::content::PackContent;
use super::report::{Diagnostic, Phase, Severity};
use crate::intelligence::{CallScope, IntelligenceClient, IntelligenceError};

/// What the installer asks the plane about one pack.
#[derive(Debug)]
pub struct CompileRequest<'a> {
    /// The tenant the pack is for.
    pub tenant_id: &'a str,
    /// The workspace the administrator acts from (every plane call carries one).
    pub workspace_id: &'a str,
    /// The command id.
    pub request_id: &'a str,
    /// The call's correlation id.
    pub correlation_id: &'a str,
    /// The content, as fetched.
    pub content: &'a PackContent,
    /// The revision the caller named, if it named one.
    pub expected_revision: Option<&'a str>,
    /// Who may propose each tool.
    pub tools: &'a [ToolVisibility],
    /// The bounds in force.
    pub limits: &'a InstallConfig,
}

/// What the plane said about a pack.
#[derive(Debug, Clone, PartialEq)]
pub struct Compilation {
    /// No error was found.
    pub ok: bool,
    /// The earliest phase with an error.
    pub failed_phase: Option<Phase>,
    /// The revision the plane computed (the runtime recomputes it and the two must agree).
    pub revision: String,
    /// What `provenance.digest` must name.
    pub manifest_digest: String,
    /// Every finding.
    pub diagnostics: Vec<Diagnostic>,
    /// What the pack resolves to.
    pub resolution: Value,
}

/// The plane cannot answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CompileUnavailable(pub String);

/// Verifies and compiles a pack for install.
#[async_trait]
pub trait PackCompilePort: Send + Sync {
    /// Ask about `request`'s pack.
    ///
    /// # Errors
    /// [`CompileUnavailable`] when no answer could be had; the install then fails at `compile`, because
    /// nothing is activated that was not compiled.
    async fn compile(&self, request: CompileRequest<'_>)
        -> Result<Compilation, CompileUnavailable>;
}

/// The settings object the plane reads.
fn settings(request: &CompileRequest<'_>) -> Value {
    let mut out = json!({
        "max_files": request.limits.max_files,
        "max_file_bytes": request.limits.max_file_bytes,
        "max_content_bytes": request.limits.max_content_bytes,
    });
    if let Some(expected) = request.expected_revision {
        out["expected_revision"] = json!(expected);
    }
    out
}

/// Read the plane's answer. Strict: it is untrusted data about the pack.
///
/// # Errors
/// Why the answer is not one.
pub fn parse_compilation(raw: &str) -> Result<Compilation, String> {
    let value: Value = serde_json::from_str(raw).map_err(|error| format!("not JSON: {error}"))?;
    let object = value.as_object().ok_or("not an object")?;
    let ok = object
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or("`ok` is not a boolean")?;
    let failed_phase = match object.get("failed_phase") {
        None | Some(Value::Null) => None,
        Some(Value::String(phase)) => match Phase::parse(phase) {
            Some(phase @ (Phase::Verify | Phase::Compile)) => Some(phase),
            _ => return Err(format!("failed_phase {phase:?} is not verify or compile")),
        },
        Some(_) => return Err("`failed_phase` is not a string".to_string()),
    };
    let text = |key: &str| -> Result<String, String> {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("`{key}` is not a string"))
    };
    let mut diagnostics = Vec::new();
    for item in object
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or("`diagnostics` is not a list")?
    {
        let phase = match item
            .get("phase")
            .and_then(Value::as_str)
            .and_then(Phase::parse)
        {
            Some(phase @ (Phase::Verify | Phase::Compile)) => phase,
            _ => return Err("a diagnostic has no verify or compile phase".to_string()),
        };
        let severity = match item.get("severity").and_then(Value::as_str) {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => return Err("a diagnostic has no known severity".to_string()),
        };
        diagnostics.push(Diagnostic {
            phase,
            rule: item
                .get("rule")
                .and_then(Value::as_str)
                .ok_or("a diagnostic has no rule")?
                .to_string(),
            severity,
            message: item
                .get("message")
                .and_then(Value::as_str)
                .ok_or("a diagnostic has no message")?
                .to_string(),
            pointer: item
                .get("pointer")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    // The plane cannot say `ok` while it reports an error, or say not-ok with none.
    let has_error = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    if ok == has_error {
        return Err("`ok` disagrees with the diagnostics".to_string());
    }
    if !ok && failed_phase.is_none() {
        return Err("a failed compilation names no phase".to_string());
    }
    Ok(Compilation {
        ok,
        failed_phase,
        revision: text("revision")?,
        manifest_digest: text("manifest_digest")?,
        diagnostics,
        resolution: object.get("resolution").cloned().unwrap_or(Value::Null),
    })
}

/// `CompilePack` over the intelligence gateway.
#[derive(Debug, Clone)]
pub struct GatewayPackCompiler {
    client: IntelligenceClient,
    deadline: Duration,
}

impl GatewayPackCompiler {
    /// A compiler that asks `client`, within `deadline`.
    #[must_use]
    pub fn new(client: IntelligenceClient, deadline: Duration) -> Self {
        Self { client, deadline }
    }
}

#[async_trait]
impl PackCompilePort for GatewayPackCompiler {
    async fn compile(
        &self,
        request: CompileRequest<'_>,
    ) -> Result<Compilation, CompileUnavailable> {
        let scope = CallScope {
            tenant_id: request.tenant_id.to_string(),
            workspace_id: request.workspace_id.to_string(),
            correlation_id: request.correlation_id.to_string(),
            deadline: self.deadline,
            capability_projection_id: "rbac:manage_tenant".to_string(),
        };
        let pack = json!({ "manifest": request.content.manifest, "files": request.content.files });
        let tools: Vec<String> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "model_visible": tool.model_visible,
                    "host_callable": tool.host_callable,
                })
                .to_string()
            })
            .collect();
        let mut client = self.client.clone();
        let raw = client
            .compile_pack(
                &scope,
                request.request_id,
                pack.to_string(),
                tools,
                settings(&request).to_string(),
            )
            .await
            .map_err(|error| match error {
                IntelligenceError::Refused { code, message } => CompileUnavailable(format!(
                    "the intelligence plane refused the pack ({code}): {message}"
                )),
                other => CompileUnavailable(format!(
                    "the intelligence plane could not be asked: {other}"
                )),
            })?;
        parse_compilation(&raw).map_err(|why| {
            CompileUnavailable(format!(
                "the intelligence plane's answer is unusable: {why}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(ok: bool, phase: Value, diagnostics: Value) -> String {
        json!({
            "schema_version": "v1", "ok": ok, "failed_phase": phase,
            "revision": "sha256:00", "manifest_digest": "sha256:00",
            "diagnostics": diagnostics, "resolution": {},
        })
        .to_string()
    }

    #[test]
    fn the_plane_answer_is_read_strictly() {
        let error = json!([{"phase": "compile", "rule": "pack.tool.host_only", "severity": "error", "message": "m"}]);
        let parsed =
            parse_compilation(&answer(false, json!("compile"), error.clone())).expect("parses");
        assert_eq!(parsed.failed_phase, Some(Phase::Compile));
        assert_eq!(parsed.diagnostics[0].rule, "pack.tool.host_only");
        // ok with an error, not-ok with none, an unknown phase or severity: all refused.
        assert!(parse_compilation(&answer(true, Value::Null, error.clone())).is_err());
        assert!(parse_compilation(&answer(false, json!("compile"), json!([]))).is_err());
        assert!(parse_compilation(&answer(false, json!("activate"), error)).is_err());
        let odd = json!([{"phase": "compile", "rule": "r", "severity": "fatal", "message": "m"}]);
        assert!(parse_compilation(&answer(false, json!("compile"), odd)).is_err());
        assert!(parse_compilation("not json").is_err());
        assert!(parse_compilation(&answer(true, Value::Null, json!([]))).is_ok());
    }
}
