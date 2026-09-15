//! Teammate definitions and roster (APP-014).
//!
//! Standing instructions are text. They cannot add grants. Archiving suspends the
//! teammate without deleting thread/work/evidence identities.

use serde_json::Value;

/// A proposed capability grant list.
pub fn standing_instructions_cannot_widen(current_grants: &[String], proposed: &[String]) -> bool {
    proposed.iter().all(|grant| current_grants.contains(grant))
}

/// Archive result: identity retained, status archived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedTeammate {
    /// `agt_` identity, still addressable.
    pub id: String,
    /// Status after archive.
    pub status: &'static str,
    /// Bound AgentThread, if any, is suspended not deleted.
    pub agent_thread_id: Option<String>,
    /// Thread/work/evidence ids remain.
    pub lineage_retained: bool,
}

/// Archive without dropping lineage.
#[must_use]
pub fn archive(id: &str, agent_thread_id: Option<String>) -> ArchivedTeammate {
    ArchivedTeammate {
        id: id.to_string(),
        status: "archived",
        agent_thread_id,
        lineage_retained: true,
    }
}

/// Load default templates from the shipped config (tests embed the same file).
pub fn templates_yaml() -> &'static str {
    include_str!("../../../../../config/teammates.yaml")
}

/// Parse template ids from the YAML blob.
pub fn template_ids(yaml: &str) -> Vec<String> {
    let parsed: Value = serde_yaml::from_str(yaml).unwrap_or(Value::Null);
    parsed
        .get("templates")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
