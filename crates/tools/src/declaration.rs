//! The Tool contract (DOMAIN.md §7.5) and its fail-closed validation.
//!
//! A declaration is control-plane data: it names the tool, fixes its argument and output
//! schemas, and states how a call derives its effect class, resource, idempotency key,
//! host, output bound and evidence capture. [`ToolDeclaration::from_raw`] refuses a
//! declaration that is incomplete or inconsistent, so a registry can never expose a tool
//! whose governance inputs are unknown.

use std::fmt;

use quansio_capability::selector::SELECTOR_KINDS;
use quansio_capability::{EffectClass, Grant};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ToolError;
use crate::schema::check_vocabulary;

/// Where a tool executes (DOMAIN.md §7.5 `host`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolHost {
    /// The trusted server runtime itself.
    Server,
    /// A local worker daemon over the machine gateway.
    Qworkerd,
    /// A browser session hosted by the runtime.
    Browser,
    /// A connector or provider adapter behind the egress boundary.
    Adapter,
}

impl ToolHost {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Qworkerd => "qworkerd",
            Self::Browser => "browser",
            Self::Adapter => "adapter",
        }
    }

    /// Parse the canonical spelling.
    ///
    /// # Errors
    /// Returns the offending value when it is not a host.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "server" => Ok(Self::Server),
            "qworkerd" => Ok(Self::Qworkerd),
            "browser" => Ok(Self::Browser),
            "adapter" => Ok(Self::Adapter),
            other => Err(format!(
                "host {other:?} is not one of server, qworkerd, browser, adapter"
            )),
        }
    }
}

impl fmt::Display for ToolHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The trust level assigned to a tool's results (DOMAIN.md §12).
///
/// Mirrors the generated contract `quansio.v1.trust.TrustLevel` without depending on the
/// protobuf bindings; `crates/tools/tests/source_trust_parity.rs` fails if the two
/// vocabularies diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTrust {
    /// Runtime-rendered system, policy and tool definitions: instructions are honoured.
    TrustedSystem,
    /// Content authored by authenticated workspace members.
    TrustedUser,
    /// ACTIVE knowledge or skill content: guidance honoured, cannot grant.
    VerifiedKnowledge,
    /// Prior assistant or worker output: context only.
    AgentGenerated,
    /// Tool output, fetched documents, connector payloads: data only, never intent.
    UntrustedExternal,
}

impl SourceTrust {
    /// The complete ladder, most trusted first.
    pub const ALL: [Self; 5] = [
        Self::TrustedSystem,
        Self::TrustedUser,
        Self::VerifiedKnowledge,
        Self::AgentGenerated,
        Self::UntrustedExternal,
    ];

    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrustedSystem => "trusted_system",
            Self::TrustedUser => "trusted_user",
            Self::VerifiedKnowledge => "verified_knowledge",
            Self::AgentGenerated => "agent_generated",
            Self::UntrustedExternal => "untrusted_external",
        }
    }

    /// The protobuf contract spelling, used by the parity test.
    #[must_use]
    pub const fn contract_name(self) -> &'static str {
        match self {
            Self::TrustedSystem => "TRUST_LEVEL_TRUSTED_SYSTEM",
            Self::TrustedUser => "TRUST_LEVEL_TRUSTED_USER",
            Self::VerifiedKnowledge => "TRUST_LEVEL_VERIFIED_KNOWLEDGE",
            Self::AgentGenerated => "TRUST_LEVEL_AGENT_GENERATED",
            Self::UntrustedExternal => "TRUST_LEVEL_UNTRUSTED_EXTERNAL",
        }
    }

    /// The numeric rank the generated contract uses (1 = most trusted).
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::TrustedSystem => 1,
            Self::TrustedUser => 2,
            Self::VerifiedKnowledge => 3,
            Self::AgentGenerated => 4,
            Self::UntrustedExternal => 5,
        }
    }

    /// Whether a result at this level is data only and can never carry intent.
    #[must_use]
    pub const fn is_data_only(self) -> bool {
        matches!(self, Self::UntrustedExternal)
    }

    /// Parse the canonical spelling.
    ///
    /// # Errors
    /// Returns the offending value when it is not a trust level.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|level| level.as_str() == value)
            .ok_or_else(|| format!("source_trust {value:?} is not a DOMAIN.md §12 trust level"))
    }
}

impl fmt::Display for SourceTrust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An effect class a call derives from its arguments rather than from the declaration.
///
/// A named derivation is implemented in code (DOMAIN.md §7.5 "static or derived by
/// function of args"); the declaration only names it, and an unknown name fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedDerivation {
    /// `fs.write`: inside the run's execution-target root is `fs.write.workspace`,
    /// outside it is `fs.write.host`.
    FsWriteScope,
    /// `scm.git`: a push to the run's own branch is `scm.remote.write`; the branch scope
    /// only ever narrows, never widens, the class.
    ScmRemoteScope,
}

impl NamedDerivation {
    /// Every derivation the runtime implements.
    pub const ALL: [Self; 2] = [Self::FsWriteScope, Self::ScmRemoteScope];

    /// The canonical spelling used in `config/tools.yaml`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FsWriteScope => "fs_write_scope",
            Self::ScmRemoteScope => "scm_remote_scope",
        }
    }

    /// The effect classes this derivation can produce.
    #[must_use]
    pub const fn candidate_effect_classes(self) -> &'static [&'static str] {
        match self {
            Self::FsWriteScope => &["fs.write.workspace", "fs.write.host"],
            Self::ScmRemoteScope => &["scm.remote.write"],
        }
    }

    /// Parse the canonical spelling.
    ///
    /// # Errors
    /// Returns the offending value when it is not a derivation.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|derivation| derivation.as_str() == value)
            .ok_or_else(|| format!("effect_class derivation {value:?} is not implemented"))
    }
}

/// How a call's effect class is determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectClassRule {
    /// The declaration fixes the class.
    Static(EffectClass),
    /// The runtime derives the class from the arguments and context.
    Derived(NamedDerivation),
}

/// How a call's resource selector is determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceRule {
    /// The declaration fixes the selector.
    Static {
        /// Selector kind (one of DOMAIN.md §6.1).
        kind: String,
        /// The selector pattern.
        selector: String,
    },
    /// The selector is read from a named argument field.
    Arg {
        /// Selector kind (one of DOMAIN.md §6.1).
        kind: String,
        /// Argument field holding the selector value.
        arg: String,
        /// Argument field holding a port set, for `domain` selectors.
        ports_arg: Option<String>,
    },
}

impl ResourceRule {
    /// The selector kind this rule produces.
    #[must_use]
    pub fn kind(&self) -> &str {
        match self {
            Self::Static { kind, .. } | Self::Arg { kind, .. } => kind,
        }
    }
}

/// Which arguments form a call's parameter digest and idempotency key (DOMAIN.md §7.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRule {
    /// Argument fields, in order, whose canonical JSON is the parameter digest.
    pub digest_args: Vec<String>,
}

/// What a tool records as evidence (DOMAIN.md §10.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceCapture {
    /// Evidence kinds the host records for a call.
    pub kinds: Vec<String>,
    /// Argument fields that must be redacted before evidence is stored.
    pub redact: Vec<String>,
}

/// One versioned Tool declaration (DOMAIN.md §7.5).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDeclaration {
    /// Namespaced tool name.
    pub name: String,
    /// Declaration version; a call resolves to the latest registered version.
    pub version: u32,
    /// Human-readable description.
    pub description: String,
    /// Strict JSON Schema for the arguments (`additionalProperties = false`).
    pub input_schema: Value,
    /// Strict JSON Schema for the bounded output.
    pub output_schema: Value,
    /// How the effect class is determined.
    pub effect_class: EffectClassRule,
    /// How the resource selector is determined.
    pub resource: ResourceRule,
    /// Which arguments form the parameter digest.
    pub idempotency: IdempotencyRule,
    /// Where the tool executes.
    pub host: ToolHost,
    /// Trust level assigned to the results when they re-enter context.
    pub source_trust: SourceTrust,
    /// Upper bound on the output the model may see.
    pub max_output_bytes: usize,
    /// Dispatch deadline after which the outcome is unknown.
    pub timeout_ms: u64,
    /// Whether a dispatched call may be cancelled.
    pub cancellable: bool,
    /// Evidence the host records.
    pub evidence: EvidenceCapture,
    /// Grants that must be present in a projection for the tool to be exposed.
    pub grant_templates: Vec<Grant>,
}

impl ToolDeclaration {
    /// Validate a raw declaration and build the contract.
    ///
    /// # Errors
    /// Returns [`ToolError::MalformedDeclaration`] for the first contract it breaks and
    /// [`ToolError::UnsupportedSchemaKeyword`] when a schema uses unknown vocabulary.
    pub fn from_raw(raw: RawToolDeclaration) -> Result<Self, ToolError> {
        let tool = raw.name.clone();
        let malformed = |detail: String| ToolError::MalformedDeclaration {
            tool: tool.clone(),
            detail,
        };

        if !is_canonical_tool_name(&raw.name) {
            return Err(malformed(
                "name must be a lowercase dotted namespace such as fs.read".to_string(),
            ));
        }
        if raw.version == 0 {
            return Err(malformed("version must be at least 1".to_string()));
        }
        if raw.description.trim().is_empty() {
            return Err(malformed("description must not be empty".to_string()));
        }
        if raw.max_output_bytes == 0 {
            return Err(malformed(
                "max_output_bytes must be greater than 0".to_string(),
            ));
        }
        if raw.timeout_ms == 0 {
            return Err(malformed("timeout_ms must be greater than 0".to_string()));
        }
        check_vocabulary(&tool, &raw.input_schema)?;
        check_vocabulary(&tool, &raw.output_schema)?;

        let effect_class = parse_effect_class_rule(&tool, &raw.effect_class)?;
        let resource = parse_resource_rule(&tool, &raw.resource)?;
        let idempotency = parse_idempotency(&tool, raw.idempotency)?;
        let host = ToolHost::parse(&raw.host).map_err(&malformed)?;
        let source_trust = SourceTrust::parse(&raw.source_trust).map_err(&malformed)?;
        let evidence = EvidenceCapture {
            kinds: raw.evidence.kinds,
            redact: raw.evidence.redact,
        };
        let grant_templates = parse_grant_templates(&tool, &raw.grant_templates)?;
        if grant_templates.is_empty() {
            return Err(malformed(
                "grant_templates must declare the grants that expose the tool".to_string(),
            ));
        }
        let candidates = candidate_effect_classes(&effect_class);
        for grant in &grant_templates {
            if !candidates.iter().any(|class| class == &grant.effect_class) {
                return Err(malformed(format!(
                    "grant template class {} is not produced by effect_class rule",
                    grant.effect_class
                )));
            }
            if grant.resource.kind() != resource.kind() {
                return Err(malformed(format!(
                    "grant template selector kind {} does not match resource rule kind {}",
                    grant.resource.kind(),
                    resource.kind()
                )));
            }
        }

        Ok(Self {
            name: raw.name,
            version: raw.version,
            description: raw.description,
            input_schema: raw.input_schema,
            output_schema: raw.output_schema,
            effect_class,
            resource,
            idempotency,
            host,
            source_trust,
            max_output_bytes: raw.max_output_bytes,
            timeout_ms: raw.timeout_ms,
            cancellable: raw.cancellable,
            evidence,
            grant_templates,
        })
    }

    /// Whether one grant in a projection can expose this tool.
    ///
    /// The declaration's grant templates state the *minimum* authority the tool needs;
    /// a projection exposes the tool when it holds a grant of the same effect class whose
    /// resource region overlaps the tool's declared region.
    #[must_use]
    pub fn covered_by(&self, grants: &[Grant], now: chrono::DateTime<chrono::Utc>) -> bool {
        self.grant_templates.iter().any(|template| {
            grants.iter().any(|grant| {
                grant.effect_class == template.effect_class
                    && grant.resource.overlaps(&template.resource)
                    && grant
                        .constraints
                        .expires_at
                        .is_none_or(|expires| expires > now)
            })
        })
    }
}

fn candidate_effect_classes(rule: &EffectClassRule) -> Vec<EffectClass> {
    match rule {
        EffectClassRule::Static(class) => vec![class.clone()],
        EffectClassRule::Derived(derivation) => derivation
            .candidate_effect_classes()
            .iter()
            .map(|name| EffectClass::parse(*name).expect("DOMAIN §7.1 class is canonical"))
            .collect(),
    }
}

fn is_canonical_tool_name(name: &str) -> bool {
    let mut segments = name.split('.');
    let Some(head) = segments.next() else {
        return false;
    };
    if head.is_empty()
        || !head
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    let mut rest = 0;
    for segment in segments {
        rest += 1;
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return false;
        }
    }
    rest > 0
}

fn parse_effect_class_rule(tool: &str, raw: &str) -> Result<EffectClassRule, ToolError> {
    if let Ok(class) = EffectClass::parse(raw) {
        return Ok(EffectClassRule::Static(class));
    }
    match NamedDerivation::parse(raw) {
        Ok(derivation) => Ok(EffectClassRule::Derived(derivation)),
        Err(detail) => Err(ToolError::MalformedDeclaration {
            tool: tool.to_string(),
            detail,
        }),
    }
}

fn parse_resource_rule(tool: &str, raw: &RawResourceRule) -> Result<ResourceRule, ToolError> {
    if !SELECTOR_KINDS.contains(&raw.kind.as_str()) {
        return Err(ToolError::MalformedDeclaration {
            tool: tool.to_string(),
            detail: format!(
                "resource kind {:?} is not one of {}",
                raw.kind,
                SELECTOR_KINDS.join(", ")
            ),
        });
    }
    let rule = match (&raw.selector, &raw.arg) {
        (Some(selector), None) => ResourceRule::Static {
            kind: raw.kind.clone(),
            selector: selector.clone(),
        },
        (None, Some(arg)) => ResourceRule::Arg {
            kind: raw.kind.clone(),
            arg: arg.clone(),
            ports_arg: raw.ports_arg.clone(),
        },
        _ => {
            return Err(ToolError::MalformedDeclaration {
                tool: tool.to_string(),
                detail: "resource needs exactly one of selector or arg".to_string(),
            })
        }
    };
    Ok(rule)
}

fn parse_idempotency(tool: &str, raw: RawIdempotencyRule) -> Result<IdempotencyRule, ToolError> {
    if raw.digest_args.is_empty() {
        return Err(ToolError::MalformedDeclaration {
            tool: tool.to_string(),
            detail: "idempotency.digest_args must name the arguments that form the digest"
                .to_string(),
        });
    }
    Ok(IdempotencyRule {
        digest_args: raw.digest_args,
    })
}

fn parse_grant_templates(tool: &str, raw: &[Value]) -> Result<Vec<Grant>, ToolError> {
    raw.iter()
        .map(|value| {
            Grant::from_json(value).map_err(|error| ToolError::MalformedDeclaration {
                tool: tool.to_string(),
                detail: format!("grant template: {error}"),
            })
        })
        .collect()
}

/// The YAML/JSON shape of a declaration in `config/tools.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawToolDeclaration {
    /// Namespaced tool name.
    pub name: String,
    /// Declaration version.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Strict input schema.
    pub input_schema: Value,
    /// Strict output schema.
    pub output_schema: Value,
    /// Effect class, either a DOMAIN §7.1 class or a named derivation.
    pub effect_class: String,
    /// Resource derivation.
    pub resource: RawResourceRule,
    /// Idempotency derivation.
    #[serde(default)]
    pub idempotency: RawIdempotencyRule,
    /// Execution host.
    pub host: String,
    /// Trust level assigned to results.
    pub source_trust: String,
    /// Output bound.
    pub max_output_bytes: usize,
    /// Dispatch deadline.
    pub timeout_ms: u64,
    /// Whether a call may be cancelled.
    #[serde(default = "default_true")]
    pub cancellable: bool,
    /// Evidence capture.
    #[serde(default)]
    pub evidence: RawEvidenceCapture,
    /// Grants that expose the tool.
    #[serde(default)]
    pub grant_templates: Vec<Value>,
}

/// The YAML/JSON shape of a resource rule.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawResourceRule {
    /// Selector kind.
    pub kind: String,
    /// Fixed selector, for a static rule.
    #[serde(default)]
    pub selector: Option<String>,
    /// Argument field holding the selector, for an argument-driven rule.
    #[serde(default)]
    pub arg: Option<String>,
    /// Argument field holding a port set.
    #[serde(default)]
    pub ports_arg: Option<String>,
}

/// The YAML/JSON shape of an idempotency rule.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawIdempotencyRule {
    /// Argument fields forming the parameter digest.
    #[serde(default)]
    pub digest_args: Vec<String>,
}

/// The YAML/JSON shape of evidence capture.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawEvidenceCapture {
    /// Evidence kinds recorded.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Argument fields redacted before storage.
    #[serde(default)]
    pub redact: Vec<String>,
}

fn default_version() -> u32 {
    1
}

fn default_true() -> bool {
    true
}
