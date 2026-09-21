//! Teammate definitions and roster (APP-014, DOMAIN.md §3 Teammate).
//!
//! A teammate is a persistent agent *definition*: persona, standing instructions, default
//! capability needs, memory scope, model route preference, default execution target and
//! availability. It is what a person creates, edits and archives; the runtime gives it one
//! persistent AgentThread per workspace (RUN-002) and every run for that thread resolves
//! its authority from the workspace's policies *narrowed by* the definition's needs — the
//! definition is an input that can only remove (DOMAIN.md §6.2), and its text (persona,
//! instructions) is never read by the projection at all. Archiving suspends the thread
//! through RUN-002's generation-fenced transition and keeps every identity; nothing here
//! deletes. Every configuration change commits its `agent.definition_*` RuntimeEvent and
//! its AuditEntry in one transaction and bumps the definition's version, which the next
//! run's projection input names — a change takes effect on the next turn, without restart.

pub mod store;
pub mod templates;

use thiserror::Error;

pub use store::{
    NewTeammate, Teammate, TeammateAdmin, TeammateChange, TeammateStore, TurnDefinition,
    TEAMMATE_OWNER,
};
pub use templates::{parse_templates, template_ids, templates, templates_yaml, Template};

/// Memory scopes a definition may name (DOMAIN.md §11.4).
pub const MEMORY_SCOPES: [&str; 3] = ["user", "workspace", "teammate"];
/// Availability states.
pub const AVAILABILITIES: [&str; 2] = ["available", "away"];

/// A teammate control-plane refusal.
#[derive(Debug, Error)]
pub enum TeammateError {
    /// The shipped or supplied configuration is not the shape the owner accepts.
    #[error("teammate configuration: {0}")]
    Config(String),
    /// A field is outside its vocabulary.
    #[error("teammate schema: {0}")]
    Schema(String),
    /// The teammate is not in this tenant/workspace.
    #[error("teammate {0} not found")]
    NotFound(String),
    /// The change contradicts the current state.
    #[error("teammate conflict: {0}")]
    Conflict(String),
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store refused the transaction that carried the mutation.
    #[error(transparent)]
    Event(#[from] quansio_events::EventError),
    /// The AgentGraph refused a thread transition.
    #[error(transparent)]
    Runtime(#[from] crate::runtime::state_machine::RuntimeError),
}

impl TeammateError {
    /// The DOMAIN.md §15 code this refusal maps to.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config(_) | Self::Database(_) | Self::Event(_) => "INTERNAL",
            Self::Schema(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT_STATE",
            Self::Runtime(error) => error.code(),
        }
    }
}

/// Refuse a memory scope outside the vocabulary.
///
/// # Errors
/// [`TeammateError::Schema`].
pub fn validate_memory_scope(value: &str) -> Result<(), TeammateError> {
    if MEMORY_SCOPES.contains(&value) {
        Ok(())
    } else {
        Err(TeammateError::Schema(format!(
            "memory_scope must be one of {MEMORY_SCOPES:?}, not {value:?}"
        )))
    }
}

/// Refuse an availability outside the vocabulary.
///
/// # Errors
/// [`TeammateError::Schema`].
pub fn validate_availability(value: &str) -> Result<(), TeammateError> {
    if AVAILABILITIES.contains(&value) {
        Ok(())
    } else {
        Err(TeammateError::Schema(format!(
            "availability must be one of {AVAILABILITIES:?}, not {value:?}"
        )))
    }
}

/// Refuse a capability need that is not an effect class.
///
/// # Errors
/// [`TeammateError::Schema`].
pub fn validate_needs(needs: &[String]) -> Result<(), TeammateError> {
    for need in needs {
        if quansio_capability::EffectClass::parse(need).is_err() {
            return Err(TeammateError::Schema(format!(
                "{need:?} is not an effect class"
            )));
        }
    }
    Ok(())
}

/// The grants a definition leaves of a policy's: those whose effect class the definition
/// names, when it names any; every one of them when it names none (no narrowing). A need
/// the policy does not grant adds nothing — a definition narrows, never widens.
#[must_use]
pub fn narrow_by_needs(
    grants: Vec<quansio_capability::Grant>,
    needs: &[String],
) -> Vec<quansio_capability::Grant> {
    if needs.is_empty() {
        return grants;
    }
    grants
        .into_iter()
        .filter(|grant| needs.iter().any(|need| need == grant.effect_class.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use quansio_capability::Grant;

    fn grant(class: &str) -> Grant {
        Grant::from_json(&serde_json::json!({
            "effect_class": class,
            "resource": { "kind": "fs", "selector": "**" },
            "constraints": {},
        }))
        .expect("grant")
    }

    #[test]
    fn a_definition_narrows_and_never_widens() {
        let policy = vec![grant("read.internal"), grant("fs.write.workspace")];
        let kept = narrow_by_needs(policy.clone(), &["read.internal".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].effect_class.as_str(), "read.internal");
        // A need the policy does not grant adds nothing.
        let widened = narrow_by_needs(
            policy.clone(),
            &["read.internal".to_string(), "network.egress".to_string()],
        );
        assert_eq!(widened.len(), 1);
        // No needs: no narrowing.
        assert_eq!(narrow_by_needs(policy.clone(), &[]).len(), 2);
        assert!(validate_needs(&["not an effect class".to_string()]).is_err());
        assert!(validate_needs(&["read.internal".to_string()]).is_ok());
    }

    #[test]
    fn the_shipped_templates_parse_and_are_configuration() {
        let shipped = templates().expect("templates");
        let ids: Vec<&str> = shipped
            .iter()
            .map(|template| template.id.as_str())
            .collect();
        assert_eq!(ids, ["general-assistant", "researcher", "engineer"]);
        for template in &shipped {
            validate_needs(&template.default_capability_needs).expect("needs are effect classes");
        }
        assert!(parse_templates(
            "schema_version: v1\ntemplates:\n  - id: x\n    name: X\n    memory_scope: galaxy\n"
        )
        .is_err());
    }
}
