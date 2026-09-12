//! Projection layers and input digests (DOMAIN.md §6.2).
//!
//! The layer order is fixed: platform → tenant policy → workspace policy → user role →
//! teammate/worker definition → delegation → active skills → tool declaration →
//! execution target class → user rules. [`Layer::all`] is that order, and assembly
//! rejects any input list that is not in it.

use core::fmt;

use quansio_core::Digest;
use serde::{Deserialize, Serialize};

/// One fixed projection layer (DOMAIN.md §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Platform baseline.
    Platform,
    /// Tenant policy.
    TenantPolicy,
    /// Workspace policy.
    WorkspacePolicy,
    /// The user's role.
    UserRole,
    /// Teammate/worker definition.
    AgentDefinition,
    /// Delegation from a parent agent thread.
    Delegation,
    /// Active skills.
    ActiveSkills,
    /// Tool declaration.
    ToolDeclaration,
    /// Execution target class.
    ExecutionTargetClass,
    /// User rules.
    UserRules,
}

impl Layer {
    /// Every layer, in the fixed order of DOMAIN.md §6.2.
    #[must_use]
    pub const fn all() -> [Layer; 10] {
        [
            Self::Platform,
            Self::TenantPolicy,
            Self::WorkspacePolicy,
            Self::UserRole,
            Self::AgentDefinition,
            Self::Delegation,
            Self::ActiveSkills,
            Self::ToolDeclaration,
            Self::ExecutionTargetClass,
            Self::UserRules,
        ]
    }

    /// Position in the fixed order.
    #[must_use]
    pub const fn ordinal(self) -> usize {
        match self {
            Self::Platform => 0,
            Self::TenantPolicy => 1,
            Self::WorkspacePolicy => 2,
            Self::UserRole => 3,
            Self::AgentDefinition => 4,
            Self::Delegation => 5,
            Self::ActiveSkills => 6,
            Self::ToolDeclaration => 7,
            Self::ExecutionTargetClass => 8,
            Self::UserRules => 9,
        }
    }

    /// The canonical snake_case layer name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::TenantPolicy => "tenant_policy",
            Self::WorkspacePolicy => "workspace_policy",
            Self::UserRole => "user_role",
            Self::AgentDefinition => "agent_definition",
            Self::Delegation => "delegation",
            Self::ActiveSkills => "active_skills",
            Self::ToolDeclaration => "tool_declaration",
            Self::ExecutionTargetClass => "execution_target_class",
            Self::UserRules => "user_rules",
        }
    }

    /// The matching value in the generated `quansio.v1.capability.ProjectionInput.Layer`
    /// contract; a test asserts the two cannot drift.
    #[must_use]
    pub const fn contract_name(self) -> &'static str {
        match self {
            Self::Platform => "LAYER_PLATFORM",
            Self::TenantPolicy => "LAYER_TENANT_POLICY",
            Self::WorkspacePolicy => "LAYER_WORKSPACE_POLICY",
            Self::UserRole => "LAYER_USER_ROLE",
            Self::AgentDefinition => "LAYER_AGENT_DEFINITION",
            Self::Delegation => "LAYER_DELEGATION",
            Self::ActiveSkills => "LAYER_ACTIVE_SKILLS",
            Self::ToolDeclaration => "LAYER_TOOL_DECLARATION",
            Self::ExecutionTargetClass => "LAYER_EXECUTION_TARGET_CLASS",
            Self::UserRules => "LAYER_USER_RULES",
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One layer's contribution to a projection (DOMAIN.md §6.2 `inputs[]`).
///
/// Serialization is manual (see [`crate::projection`]) because [`Digest`] carries no
/// serde implementation; the wire shape is `{layer, ref, digest}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectionInput {
    /// The layer.
    pub layer: Layer,
    /// The canonical reference of the layer source (policy id, skill version, …).
    pub reference: String,
    /// SHA-256 of the layer source content.
    pub digest: Digest,
}

impl ProjectionInput {
    /// Build an input record.
    #[must_use]
    pub fn new(layer: Layer, reference: impl Into<String>, digest: Digest) -> Self {
        Self {
            layer,
            reference: reference.into(),
            digest,
        }
    }
}

/// Canonical digest over an ordered input list (DOMAIN.md §6.2 `inputs_digest`).
///
/// The digest is over the exact `(layer, ref, digest)` triples in order, so any change
/// to a layer's source, order or presence changes it. This is what a stale projection
/// check compares against the current inputs.
#[must_use]
pub fn inputs_digest(inputs: &[ProjectionInput]) -> Digest {
    let entries: Vec<serde_json::Value> = inputs
        .iter()
        .map(|input| {
            let mut object = serde_json::Map::new();
            object.insert(
                "digest".to_string(),
                serde_json::Value::String(input.digest.to_string()),
            );
            object.insert(
                "layer".to_string(),
                serde_json::Value::String(input.layer.as_str().to_string()),
            );
            object.insert(
                "ref".to_string(),
                serde_json::Value::String(input.reference.clone()),
            );
            serde_json::Value::Object(object)
        })
        .collect();
    let canonical =
        serde_json::to_string(&serde_json::Value::Array(entries)).expect("inputs serialize");
    Digest::of_canonical_json(&canonical)
}

/// Whether an input list is in the fixed layer order, with no layer repeated.
#[must_use]
pub fn inputs_in_canonical_order(inputs: &[ProjectionInput]) -> bool {
    inputs
        .iter()
        .map(|input| input.layer.ordinal())
        .try_fold(None, |previous: Option<usize>, ordinal| match previous {
            Some(previous) if ordinal <= previous => None,
            _ => Some(Some(ordinal)),
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACT: &str = include_str!("../../contracts/src/generated/quansio.v1.capability.rs");

    #[test]
    fn layer_order_and_names_match_the_generated_contract() {
        assert_eq!(Layer::all().len(), 10);
        let mut previous = 0usize;
        for layer in Layer::all() {
            let name = layer.contract_name();
            let position = CONTRACT
                .find(name)
                .unwrap_or_else(|| panic!("{name} is missing from the capability contract"));
            assert!(position >= previous, "{name} is out of contract order");
            previous = position;
        }
    }

    #[test]
    fn layer_ordinals_are_dense_and_ordered() {
        for (index, layer) in Layer::all().into_iter().enumerate() {
            assert_eq!(layer.ordinal(), index);
        }
    }

    #[test]
    fn inputs_digest_changes_with_order_and_content() {
        let first = ProjectionInput::new(Layer::Platform, "platform/1", Digest::of(b"one"));
        let second = ProjectionInput::new(Layer::UserRole, "role/admin/1", Digest::of(b"two"));
        let forward = inputs_digest(&[first.clone(), second.clone()]);
        let reversed = inputs_digest(&[second, first.clone()]);
        let changed = inputs_digest(&[
            first,
            ProjectionInput::new(Layer::UserRole, "role/other/1", Digest::of(b"two")),
        ]);
        assert_ne!(forward, reversed);
        assert_ne!(forward, changed);
    }

    #[test]
    fn canonical_order_rejects_disorder_and_duplicates() {
        let platform = ProjectionInput::new(Layer::Platform, "platform/1", Digest::of(b"one"));
        let role = ProjectionInput::new(Layer::UserRole, "role/admin/1", Digest::of(b"two"));
        assert!(inputs_in_canonical_order(&[]));
        assert!(inputs_in_canonical_order(&[platform.clone(), role.clone()]));
        assert!(!inputs_in_canonical_order(&[
            role.clone(),
            platform.clone()
        ]));
        assert!(!inputs_in_canonical_order(&[platform.clone(), platform]));
    }
}
