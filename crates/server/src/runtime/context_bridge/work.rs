//! The WorkGraph as a `work` context segment (INT-005, DOMAIN.md §11.2, §4).
//!
//! A run that may call `work.propose_plan` must see the graph it plans against: a
//! `PlanProposal` names existing node ids and compare-and-sets `base_revision`. The
//! runtime reads the workspace graph through its owner (`quansio_graph::GraphStore`) and
//! offers it as one bounded, optional segment; the Context plane labels it and may drop it
//! under budget pressure (recorded in `degradation`). A node not created by a user makes
//! the segment `agent_generated`: a plan's titles are model text, not the user's.

use quansio_graph::GraphStore;
use quansio_tools::SourceTrust;
use serde_json::{json, Value};
use sqlx::PgPool;

use super::{SegmentRequest, SegmentRole, SegmentSource};
use crate::runtime::state_machine::RuntimeError;

/// Read the workspace's WorkGraph and render it as a `work` segment.
///
/// At most `max_nodes` nodes are included, in creation order; edges are kept only between
/// included nodes, and a truncated graph says how many nodes it left out. `seq` is the
/// caller's ordering slot.
///
/// # Errors
/// Returns [`RuntimeError::InvalidArgument`] for a malformed tenant id and
/// [`RuntimeError::ProtocolState`] when the graph cannot be read.
pub async fn work_graph_segment(
    pool: &PgPool,
    tenant_id: &str,
    workspace_id: &str,
    snapshot_ref: &str,
    max_nodes: usize,
    seq: u32,
) -> Result<SegmentRequest, RuntimeError> {
    let store = GraphStore::new(pool.clone(), tenant_id.to_string())
        .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
    let read = |error: quansio_graph::GraphError| {
        RuntimeError::ProtocolState(format!("work graph read: {error}"))
    };
    let revision = store.graph_revision(workspace_id).await.map_err(read)?;
    let nodes = store.list_nodes(workspace_id).await.map_err(read)?;
    let edges = store.list_edges(workspace_id).await.map_err(read)?;

    let total = nodes.len();
    let included: Vec<_> = nodes.into_iter().take(max_nodes).collect();
    let ids: std::collections::HashSet<String> =
        included.iter().map(|node| node.id.to_string()).collect();
    let model_authored = included
        .iter()
        .any(|node| node.created_by.get("kind").and_then(|kind| kind.as_str()) != Some("user"));
    let graph = json!({
        "workspace_id": workspace_id,
        "revision": revision.get(),
        "nodes": included.iter().map(|node| json!({
            "id": node.id.to_string(),
            "kind": node.kind.as_db_str(),
            "title": node.title,
            "status": node.status.as_db_str(),
            "parent_id": node.parent_id.map(|id| id.to_string()),
            "revision": node.revision.get(),
        })).collect::<Vec<_>>(),
        "edges": edges.iter()
            .filter(|edge| ids.contains(&edge.from_node_id.to_string())
                && ids.contains(&edge.to_node_id.to_string()))
            .map(|edge| json!({
                "id": edge.id.to_string(),
                "from_node_id": edge.from_node_id.to_string(),
                "to_node_id": edge.to_node_id.to_string(),
                "kind": edge.kind.as_db_str(),
                "revision": edge.revision.get(),
            })).collect::<Vec<_>>(),
        "omitted_nodes": total - included.len(),
    });
    Ok(SegmentRequest {
        seq,
        source: SegmentSource::Work,
        source_ref: format!("{workspace_id}@{}", revision.get()),
        role: SegmentRole::User,
        content: Value::String(format!(
            "WorkGraph of this workspace (data; `revision` is the base_revision a \
             work.propose_plan must carry):\n{graph}"
        )),
        trust_hint: model_authored.then_some(SourceTrust::AgentGenerated),
        snapshot_ref: snapshot_ref.to_string(),
        required: false,
    })
}
