//! Typed refusals from the Tool contract and Tool Registry (DOMAIN.md §7.5).
//!
//! Every refusal is fail-closed: an unregistered tool, an unknown argument field, an
//! unsupported schema keyword or a capability that does not cover the call is rejected
//! before anything is dispatched.

use thiserror::Error;

/// A Tool contract, registry or argument validation failure.
#[derive(Debug, Error)]
pub enum ToolError {
    /// No declaration is registered under the proposed tool name.
    #[error("tool {name} is not registered")]
    UnknownTool {
        /// Proposed tool name.
        name: String,
    },
    /// The tool exists but not at the proposed version.
    #[error("tool {name} version {version} is not registered")]
    UnknownToolVersion {
        /// Proposed tool name.
        name: String,
        /// Proposed declaration version.
        version: u32,
    },
    /// A declaration does not satisfy the Tool contract.
    #[error("tool declaration {tool} is malformed: {detail}")]
    MalformedDeclaration {
        /// Declaration name.
        tool: String,
        /// What the contract requires.
        detail: String,
    },
    /// A declaration's schema uses a keyword the strict validator does not implement.
    ///
    /// The validator refuses unsupported vocabulary rather than ignoring it, so a schema
    /// can never be silently under-enforced.
    #[error("tool {tool} schema uses unsupported keyword {keyword}")]
    UnsupportedSchemaKeyword {
        /// Declaration name.
        tool: String,
        /// The keyword that is not implemented.
        keyword: String,
    },
    /// Proposed arguments do not satisfy the declaration's input schema.
    #[error("tool {tool} arguments are invalid at {path}: {detail}")]
    InvalidArguments {
        /// Tool name.
        tool: String,
        /// Instance location of the first violation.
        path: String,
        /// What the schema required.
        detail: String,
    },
    /// The Tool Registry configuration could not be read or parsed.
    #[error("tool registry config error: {0}")]
    Config(String),
    /// The registry and the generated tool catalog (`schemas/catalog/tools.yaml`) diverge.
    #[error("tool catalog divergence: {detail}")]
    CatalogDivergence {
        /// Which direction diverged and on which name.
        detail: String,
    },
    /// The tool is not exposed by the subject's capability projection.
    #[error("tool {tool} is not in the capability projection")]
    NotExposed {
        /// Tool name.
        tool: String,
    },
    /// A tool proposal, or a tool declaration, names an effect class with no catalog tier.
    #[error("tool {tool}: effect class {effect_class} has no catalog tier")]
    UnknownEffectClass {
        /// Tool name.
        tool: String,
        /// The class with no tier.
        effect_class: String,
    },
    /// The capability projection explicitly refuses the derived effect.
    #[error("tool {tool} is denied by the capability projection: {detail}")]
    CapabilityDenied {
        /// Tool name.
        tool: String,
        /// Why the projection refused.
        detail: String,
    },
}

impl ToolError {
    /// The Quansio error code (DOMAIN.md §15) this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownTool { .. }
            | Self::UnknownToolVersion { .. }
            | Self::MalformedDeclaration { .. }
            | Self::UnsupportedSchemaKeyword { .. }
            | Self::InvalidArguments { .. } => "VALIDATION_SCHEMA",
            Self::NotExposed { .. } | Self::CapabilityDenied { .. } => "CAPABILITY_DENIED",
            Self::Config(_) | Self::CatalogDivergence { .. } | Self::UnknownEffectClass { .. } => {
                "INTERNAL"
            }
        }
    }
}
