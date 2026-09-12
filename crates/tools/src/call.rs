//! Validating and planning one proposed tool call (DOMAIN.md §7.4, §7.5).
//!
//! [`plan_call`] is the pre-dispatch gate: it resolves the declaration, validates the
//! arguments strictly, derives the effect class, resource selector, parameter digest and
//! idempotency key, and reports the host, output bound, evidence capture and source trust.
//! Nothing is dispatched and no effect is reserved here — the caller does that only after
//! this function returns a plan.

use quansio_capability::{EffectClass, ResourceSelector, Tier};
use quansio_core::{Digest, IdempotencyKey};
use serde_json::Value;

use crate::canonical::canonical_json;
use crate::declaration::{
    EffectClassRule, NamedDerivation, ResourceRule, SourceTrust, ToolDeclaration, ToolHost,
};
use crate::error::ToolError;
use crate::registry::ToolRegistry;
use crate::schema::first_violation;

/// What the runtime knows about the call site, used by context-sensitive derivations.
#[derive(Debug, Clone, Default)]
pub struct CallContext<'a> {
    /// Workspace the run belongs to.
    pub workspace_id: &'a str,
    /// Run that proposed the call, when it is a run-scoped call.
    pub run_id: Option<&'a str>,
    /// Execution-target worksapce root, if the run has one.
    pub fs_root: Option<&'a str>,
    /// The branch the run owns, for source-control derivations.
    pub own_branch: Option<&'a str>,
}

/// A validated, authorized-ready plan for one tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallPlan {
    /// Resolved tool name.
    pub tool: String,
    /// Resolved declaration version.
    pub version: u32,
    /// Where the call executes.
    pub host: ToolHost,
    /// Effect class the arguments derive.
    pub effect_class: EffectClass,
    /// Consequence tier from the effect taxonomy.
    pub tier: Tier,
    /// Resource region the call touches.
    pub resource: ResourceSelector,
    /// Digest over the declaration's `digest_args`.
    pub params_digest: Digest,
    /// Effect idempotency key derived from class, resource and digest (DOMAIN.md §7.2).
    pub idempotency_key: IdempotencyKey,
    /// The validated arguments.
    pub args: Value,
    /// Trust level assigned to the result when it re-enters context.
    pub source_trust: SourceTrust,
    /// Upper bound on output returned to the model.
    pub max_output_bytes: usize,
    /// Dispatch deadline.
    pub timeout_ms: u64,
    /// Whether the call may be cancelled.
    pub cancellable: bool,
    /// Argument fields redacted before evidence is stored.
    pub redact: Vec<String>,
    /// Evidence kinds the host records.
    pub evidence_kinds: Vec<String>,
}

/// Resolve, validate and derive one proposed call.
///
/// `tier_of` resolves an effect class to its DOMAIN.md §7.1 tier; a class with no tier
/// fails closed.
///
/// # Errors
/// Returns [`ToolError::UnknownTool`] or [`ToolError::UnknownToolVersion`] when the tool
/// is not registered, [`ToolError::InvalidArguments`] for the first schema violation
/// (an unknown field is a violation because every object schema closes its property set),
/// and [`ToolError::UnknownEffectClass`] when the derived class has no tier.
pub fn plan_call(
    registry: &ToolRegistry,
    name: &str,
    version: Option<u32>,
    args: &Value,
    tier_of: &dyn Fn(&EffectClass) -> Option<Tier>,
    context: &CallContext<'_>,
) -> Result<ToolCallPlan, ToolError> {
    let declaration =
        match version {
            Some(version) => registry.get_version(name, version).ok_or_else(|| {
                ToolError::UnknownToolVersion {
                    name: name.to_string(),
                    version,
                }
            })?,
            None => registry.get(name).ok_or_else(|| ToolError::UnknownTool {
                name: name.to_string(),
            })?,
        };

    if let Some(violation) = first_violation(&declaration.input_schema, args) {
        return Err(ToolError::InvalidArguments {
            tool: name.to_string(),
            path: violation.path,
            detail: violation.detail,
        });
    }

    let effect_class = derive_effect_class(&declaration.effect_class, args, context)?;
    let tier = tier_of(&effect_class).ok_or_else(|| ToolError::UnknownEffectClass {
        tool: name.to_string(),
        effect_class: effect_class.as_str().to_string(),
    })?;
    let resource = derive_resource(name, &declaration.resource, args)?;
    let params_digest = derive_params_digest(declaration, args)?;
    let idempotency_key = IdempotencyKey::derive(
        effect_class.as_str(),
        &resource.to_string(),
        params_digest.clone(),
    );

    Ok(ToolCallPlan {
        tool: declaration.name.clone(),
        version: declaration.version,
        host: declaration.host,
        effect_class,
        tier,
        resource,
        params_digest,
        idempotency_key,
        args: args.clone(),
        source_trust: declaration.source_trust,
        max_output_bytes: declaration.max_output_bytes,
        timeout_ms: declaration.timeout_ms,
        cancellable: declaration.cancellable,
        redact: declaration.evidence.redact.clone(),
        evidence_kinds: declaration.evidence.kinds.clone(),
    })
}

fn derive_effect_class(
    rule: &EffectClassRule,
    args: &Value,
    context: &CallContext<'_>,
) -> Result<EffectClass, ToolError> {
    match rule {
        EffectClassRule::Static(class) => Ok(class.clone()),
        EffectClassRule::Derived(NamedDerivation::FsWriteScope) => {
            // Fail closed: without a known target root the call is treated as a host write,
            // the stricter tier 3 class.
            let class = match (context.fs_root, args.get("path").and_then(Value::as_str)) {
                (Some(root), Some(path)) if path_is_inside(root, path) => "fs.write.workspace",
                _ => "fs.write.host",
            };
            Ok(EffectClass::parse(class).expect("DOMAIN §7.1 class is canonical"))
        }
        EffectClassRule::Derived(NamedDerivation::ScmRemoteScope) => {
            // Pushing to the run's own branch stays within the same class; an unknown
            // branch scope never lowers the class.
            let _own_branch = context.own_branch;
            Ok(EffectClass::parse("scm.remote.write").expect("DOMAIN §7.1 class is canonical"))
        }
    }
}

/// Whether `path` resolves inside `root` under a lexical normalisation.
///
/// A relative path is resolved against the root, which is how a workspace-scoped tool
/// reads it. A `..` that would leave the root makes the path a host write; nothing here
/// follows a symlink, so the runtime never widens a call by guessing.
fn path_is_inside(root: &str, path: &str) -> bool {
    let Some(root_segments) = normalise(root) else {
        return false;
    };
    if root_segments.is_empty() {
        return false;
    }
    let joined = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{}", root.trim_end_matches('/'), path)
    };
    match normalise(&joined) {
        Some(segments) => segments.starts_with(&root_segments),
        None => false,
    }
}

/// Lexically normalise a path into segments, or `None` when a `..` escapes the root.
fn normalise(path: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    Some(out)
}

fn derive_resource(
    tool: &str,
    rule: &ResourceRule,
    args: &Value,
) -> Result<ResourceSelector, ToolError> {
    let invalid = |path: String, detail: String| ToolError::InvalidArguments {
        tool: tool.to_string(),
        path,
        detail,
    };
    match rule {
        ResourceRule::Static { kind, selector } => {
            ResourceSelector::from_parts(kind, selector, &[]).map_err(|error| {
                ToolError::MalformedDeclaration {
                    tool: tool.to_string(),
                    detail: format!("static {kind} selector: {error}"),
                }
            })
        }
        ResourceRule::Arg {
            kind,
            arg,
            ports_arg,
        } => {
            let value = args.get(arg).and_then(Value::as_str).ok_or_else(|| {
                invalid(
                    format!(".{arg}"),
                    "resource argument must be a string".to_string(),
                )
            })?;
            let ports: Vec<u16> = match ports_arg {
                Some(name) => args
                    .get(name)
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_u64)
                            .filter_map(|port| u16::try_from(port).ok())
                            .collect()
                    })
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let selector = match kind.as_str() {
                "domain" => host_glob_from(value),
                _ => value.to_string(),
            };
            ResourceSelector::from_parts(kind, &selector, &ports)
                .map_err(|error| invalid(format!(".{arg}"), error.to_string()))
        }
    }
}

/// Reduce a URL or host argument to the host glob a `domain` selector needs.
fn host_glob_from(value: &str) -> String {
    let without_scheme = match value.split_once("://") {
        Some((_, rest)) => rest,
        None => value,
    };
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    match authority.rsplit_once('@') {
        Some((_, host)) => host.to_string(),
        None => authority.to_string(),
    }
}

fn derive_params_digest(declaration: &ToolDeclaration, args: &Value) -> Result<Digest, ToolError> {
    let mut material = serde_json::Map::new();
    for name in &declaration.idempotency.digest_args {
        let value = args.get(name).ok_or_else(|| ToolError::InvalidArguments {
            tool: declaration.name.clone(),
            path: format!(".{name}"),
            detail: "is a required digest argument".to_string(),
        })?;
        material.insert(name.clone(), value.clone());
    }
    Ok(Digest::of_canonical_json(&canonical_json(&Value::Object(
        material,
    ))))
}

#[cfg(test)]
mod tests {
    use super::{host_glob_from, path_is_inside};

    #[test]
    fn paths_inside_the_root_are_recognised() {
        assert!(path_is_inside("/work/root", "/work/root/src/main.rs"));
        assert!(path_is_inside("/work/root", "/work/root/./a/../b"));
        assert!(path_is_inside("/work/root", "src/main.rs"));
        assert!(path_is_inside("/work/root/", "src/main.rs"));
        assert!(!path_is_inside("/work/root", "/work/root/../../etc/passwd"));
        assert!(!path_is_inside("/work/root", "/etc/passwd"));
        assert!(!path_is_inside("/work/root", "../etc/passwd"));
        assert!(!path_is_inside("", "/work/root/src"));
        assert!(!path_is_inside("/", "src/main.rs"));
    }

    #[test]
    fn urls_reduce_to_their_host() {
        assert_eq!(
            host_glob_from("https://api.example.com/v1/x"),
            "api.example.com"
        );
        assert_eq!(
            host_glob_from("api.example.com:443/x"),
            "api.example.com:443"
        );
        assert_eq!(host_glob_from("user@example.com/x"), "example.com");
        assert_eq!(host_glob_from("*.example.com"), "*.example.com");
    }
}
