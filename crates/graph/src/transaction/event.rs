//! RuntimeEvent mapping for graph changes (DOMAIN.md §9.1–§9.2).
//!
//! Every change a [`GraphTransaction`](super::GraphTransaction) commits is recorded by an
//! event whose `type` is a DOMAIN.md §9.2 name, whose `aggregate_type`/`aggregate_id`
//! name the row that changed, and whose `correlation_id`/`causation_id`/`actor` come from
//! the transaction context. A change — or a status a change targets — that has no §9.2
//! name is refused *before* the transaction opens, so a committed graph mutation can
//! never lack its event.
//!
//! `aggregate_version` is the version of the aggregate after the transition. Aggregates
//! with a persisted revision (`work_nodes`, `work_edges`, the workspace graph head) report
//! that revision; the aggregates that carry no revision column (agent threads, runs)
//! report the count of their committed events plus one, read inside the same transaction,
//! so versions are monotonic and gap-free per aggregate.

use std::collections::HashMap;

use quansio_core::{Generation, Revision};
use quansio_events::{EventDraft, EventType};
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};

use super::{GraphTransactionError, TransactionContext};
use crate::batch::{AppliedChange, GraphChange};
use crate::error::GraphError;
use crate::state::{AgentThreadStatus, RunStatus};
use crate::work::WorkEdge;

/// `work.node_created`.
pub(crate) const WORK_NODE_CREATED: &str = "work.node_created";
/// `work.node_status_changed`.
pub(crate) const WORK_NODE_STATUS_CHANGED: &str = "work.node_status_changed";
/// `work.edge_added`.
pub(crate) const WORK_EDGE_ADDED: &str = "work.edge_added";
/// `work.edge_removed`.
pub(crate) const WORK_EDGE_REMOVED: &str = "work.edge_removed";
/// `work.plan_applied`.
pub(crate) const WORK_PLAN_APPLIED: &str = "work.plan_applied";
/// `agent.thread_provisioned`.
pub(crate) const AGENT_THREAD_PROVISIONED: &str = "agent.thread_provisioned";
/// `agent.delegated`.
pub(crate) const AGENT_DELEGATED: &str = "agent.delegated";
/// `run.created`.
pub(crate) const RUN_CREATED: &str = "run.created";

/// The `agent.*` name for an AgentThread status change, when DOMAIN.md §9.2 has one.
///
/// `PROVISIONED` is created, never transitioned into; `JOINING` has no §9.2 name yet, so a
/// transition into it is refused rather than recorded under an invented name.
pub(crate) const fn agent_thread_event_name(to: AgentThreadStatus) -> Option<&'static str> {
    match to {
        AgentThreadStatus::Provisioned | AgentThreadStatus::Joining => None,
        AgentThreadStatus::Active => Some("agent.activated"),
        AgentThreadStatus::Suspended => Some("agent.suspended"),
        AgentThreadStatus::HandingOff => Some("agent.handoff_started"),
        AgentThreadStatus::HandedOff => Some("agent.handoff_completed"),
        AgentThreadStatus::Joined => Some("agent.joined"),
        AgentThreadStatus::Terminated => Some("agent.terminated"),
    }
}

/// The `run.*` name for a Run status change (DOMAIN.md §9.2).
///
/// `CREATED` is the creation event; `QUEUED → RUNNING` starts the run, and any other
/// entry into `RUNNING` (from a `WAITING_*` state or from `SUSPENDED`) resumes it.
pub(crate) const fn run_event_name(from: RunStatus, to: RunStatus) -> Option<&'static str> {
    match to {
        RunStatus::Created => None,
        RunStatus::Queued => Some("run.queued"),
        RunStatus::Running => {
            if matches!(from, RunStatus::Queued) {
                Some("run.started")
            } else {
                Some("run.resumed")
            }
        }
        RunStatus::WaitingApproval
        | RunStatus::WaitingQuestion
        | RunStatus::WaitingEvent
        | RunStatus::WaitingTimer
        | RunStatus::WaitingChild
        | RunStatus::WaitingTakeover => Some("run.waiting"),
        RunStatus::Verifying => Some("run.verifying"),
        RunStatus::Succeeded => Some("run.succeeded"),
        RunStatus::Failed => Some("run.failed"),
        RunStatus::Cancelled => Some("run.cancelled"),
        RunStatus::BlockedUnrecoverable => Some("run.blocked"),
        RunStatus::Suspended => Some("run.suspended"),
    }
}

/// Refuse a change that has no RuntimeEvent mapping, before anything is written.
///
/// The refusal covers two cases: a change kind whose events belong to a later task
/// (turns, steps, attempts — DOMAIN.md §9.2 names no `turn.*`/`step.*` event yet), and a
/// status change target that §9.2 names no event for.
///
/// # Errors
/// Returns [`GraphTransactionError::UnsupportedChange`] for both cases.
pub(crate) fn check_mappable(change: &GraphChange) -> Result<(), GraphTransactionError> {
    let unmapped = |detail: String| GraphTransactionError::UnsupportedChange {
        change: change.kind(),
        detail,
    };
    match change {
        GraphChange::CreateWorkNode(_)
        | GraphChange::CreateWorkEdge(_)
        | GraphChange::RemoveWorkEdge { .. }
        | GraphChange::TransitionWorkNode { .. }
        | GraphChange::VerificationPassed { .. }
        | GraphChange::CreateAgentThread(_)
        | GraphChange::Delegate { .. }
        | GraphChange::CreateRun(_) => Ok(()),
        GraphChange::TransitionAgentThread { to, .. } => match agent_thread_event_name(*to) {
            Some(_) => Ok(()),
            None => Err(unmapped(format!(
                "DOMAIN.md §9.2 names no agent.* event for status {to:?}"
            ))),
        },
        GraphChange::TransitionRun { to, .. } => match run_event_name(RunStatus::Created, *to) {
            Some(_) => Ok(()),
            None => Err(unmapped(format!(
                "DOMAIN.md §9.2 names no run.* event for status {to:?}"
            ))),
        },
        GraphChange::CreateTurn { .. }
        | GraphChange::TransitionTurn { .. }
        | GraphChange::CreateStep { .. }
        | GraphChange::TransitionStep { .. }
        | GraphChange::StartAttempt { .. }
        | GraphChange::FinishAttempt { .. } => Err(unmapped(
            "DOMAIN.md §9.2 names no event for this change kind yet".to_string(),
        )),
    }
}

/// Per-aggregate version counter for aggregates without a persisted revision column.
#[derive(Debug, Default)]
pub(crate) struct AggregateVersions {
    versions: HashMap<(String, String), u64>,
}

impl AggregateVersions {
    /// The version of `aggregate_id` after this transition.
    ///
    /// Seeded from the committed events of that aggregate (our staged drafts are not
    /// written yet) and advanced for each further change to the same aggregate in the
    /// same transaction.
    async fn next(
        &mut self,
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> Result<u64, GraphTransactionError> {
        let key = (aggregate_type.to_string(), aggregate_id.to_string());
        if let Some(next) = self.versions.get_mut(&key) {
            *next += 1;
            return Ok(*next);
        }
        let existing: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND aggregate_type = $2 \
             AND aggregate_id = $3",
        )
        .bind(tenant_id)
        .bind(aggregate_type)
        .bind(aggregate_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(GraphError::from)?;
        let next = u64::try_from(existing).unwrap_or(0) + 1;
        self.versions.insert(key, next);
        Ok(next)
    }
}

/// Build the RuntimeEvents for one applied change.
///
/// # Errors
/// Returns a [`GraphTransactionError`] when an event type is not canonical or the
/// aggregate version cannot be read.
pub(crate) async fn drafts_for(
    applied: &AppliedChange,
    context: &TransactionContext,
    tenant_id: &str,
    tx: &mut Transaction<'static, Postgres>,
    versions: &mut AggregateVersions,
) -> Result<Vec<EventDraft>, GraphTransactionError> {
    let drafts = match applied {
        AppliedChange::WorkNodeCreated(node) => vec![event(
            context,
            "work_node",
            node.id.to_string(),
            node.revision.get(),
            WORK_NODE_CREATED,
            &node.workspace_id,
            None,
            json!({
                "node_id": node.id.to_string(),
                "workspace_id": node.workspace_id,
                "kind": node.kind.as_db_str(),
                "title": node.title,
                "status": node.status.as_db_str(),
                "parent_id": node.parent_id.as_ref().map(ToString::to_string),
                "origin": node.origin.as_db_str(),
                "revision": node.revision.get(),
            }),
        )?],
        AppliedChange::WorkNodeStatusChanged {
            node,
            from,
            verified,
        } => vec![event(
            context,
            "work_node",
            node.id.to_string(),
            node.revision.get(),
            WORK_NODE_STATUS_CHANGED,
            &node.workspace_id,
            None,
            json!({
                "node_id": node.id.to_string(),
                "from": from.as_db_str(),
                "to": node.status.as_db_str(),
                "verified": verified,
                "revision": node.revision.get(),
            }),
        )?],
        AppliedChange::WorkEdgeAdded(edge) => vec![edge_draft(
            context,
            edge,
            WORK_EDGE_ADDED,
            edge.revision.get(),
        )?],
        AppliedChange::WorkEdgeRemoved(edge) => {
            vec![edge_draft(
                context,
                edge,
                WORK_EDGE_REMOVED,
                edge.revision.get(),
            )?]
        }
        AppliedChange::AgentThreadProvisioned(thread) => vec![event(
            context,
            "agent_thread",
            thread.id.to_string(),
            1,
            AGENT_THREAD_PROVISIONED,
            &thread.workspace_id,
            Some(thread.generation),
            json!({
                "agent_thread_id": thread.id.to_string(),
                "workspace_id": thread.workspace_id,
                "agent_kind": thread.agent_kind.as_db_str(),
                "parent_id": thread.parent_id.as_ref().map(ToString::to_string),
                "definition_id": thread.definition_id.as_ref().map(ToString::to_string),
                "work_node_id": thread.work_node_id.as_ref().map(ToString::to_string),
                "status": thread.status.as_db_str(),
                "generation": thread.generation.get(),
            }),
        )?],
        AppliedChange::Delegated {
            parent_id,
            parent_generation,
            delegated,
        } => {
            let child = &delegated.child;
            let edge = &delegated.edge;
            let parent_version = versions
                .next(tx, tenant_id, "agent_thread", &parent_id.to_string())
                .await?;
            vec![
                event(
                    context,
                    "agent_thread",
                    child.id.to_string(),
                    1,
                    AGENT_THREAD_PROVISIONED,
                    &child.workspace_id,
                    Some(child.generation),
                    json!({
                        "agent_thread_id": child.id.to_string(),
                        "parent_id": child.parent_id.as_ref().map(ToString::to_string),
                        "work_node_id": child.work_node_id.as_ref().map(ToString::to_string),
                        "status": child.status.as_db_str(),
                        "generation": child.generation.get(),
                    }),
                )?,
                event(
                    context,
                    "agent_thread",
                    parent_id.to_string(),
                    parent_version,
                    AGENT_DELEGATED,
                    &child.workspace_id,
                    Some(*parent_generation),
                    json!({
                        "parent_agent_thread_id": parent_id.to_string(),
                        "child_agent_thread_id": child.id.to_string(),
                        "delegation_id": edge.id,
                        "work_node_id": edge.work_node_id.as_ref().map(ToString::to_string),
                        "delegation_capability_id": edge
                            .delegation_capability_id
                            .as_ref()
                            .map(ToString::to_string),
                    }),
                )?,
            ]
        }
        AppliedChange::AgentThreadStatusChanged { thread, from } => {
            let Some(event_type) = agent_thread_event_name(thread.status) else {
                return Err(GraphTransactionError::UnsupportedChange {
                    change: "TransitionAgentThread",
                    detail: format!(
                        "DOMAIN.md §9.2 names no agent.* event for status {:?}",
                        thread.status
                    ),
                });
            };
            let version = versions
                .next(tx, tenant_id, "agent_thread", &thread.id.to_string())
                .await?;
            vec![event(
                context,
                "agent_thread",
                thread.id.to_string(),
                version,
                event_type,
                &thread.workspace_id,
                Some(thread.generation),
                json!({
                    "agent_thread_id": thread.id.to_string(),
                    "from": from.as_db_str(),
                    "to": thread.status.as_db_str(),
                    "generation": thread.generation.get(),
                }),
            )?]
        }
        AppliedChange::RunCreated(run) => vec![event(
            context,
            "run",
            run.id.to_string(),
            1,
            RUN_CREATED,
            &run.workspace_id,
            Some(run.generation),
            json!({
                "run_id": run.id.to_string(),
                "workspace_id": run.workspace_id,
                "work_node_id": run.work_node_id.to_string(),
                "agent_thread_id": run.agent_thread_id.to_string(),
                "status": run.status.as_db_str(),
                "trigger_kind": run.trigger_kind.as_db_str(),
                "trigger_ref": run.trigger_ref,
                "generation": run.generation.get(),
            }),
        )?],
        AppliedChange::RunStatusChanged { run, from } => {
            let Some(event_type) = run_event_name(*from, run.status) else {
                return Err(GraphTransactionError::UnsupportedChange {
                    change: "TransitionRun",
                    detail: format!(
                        "DOMAIN.md §9.2 names no run.* event for status {:?}",
                        run.status
                    ),
                });
            };
            let version = versions
                .next(tx, tenant_id, "run", &run.id.to_string())
                .await?;
            vec![event(
                context,
                "run",
                run.id.to_string(),
                version,
                event_type,
                &run.workspace_id,
                Some(run.generation),
                json!({
                    "run_id": run.id.to_string(),
                    "from": from.as_db_str(),
                    "to": run.status.as_db_str(),
                    "terminal_reason": run.terminal_reason,
                    "generation": run.generation.get(),
                }),
            )?]
        }
    };
    Ok(drafts)
}

/// The `work.plan_applied` event for an accepted PlanProposal (DOMAIN.md §4.5).
///
/// The aggregate is the workspace WorkGraph, whose head revision is the graph aggregate
/// revision (DOMAIN.md §1.2), so the event carries the revision the plan produced.
///
/// # Errors
/// Returns a [`GraphTransactionError`] when the event type is not canonical.
pub(crate) fn plan_applied_draft(
    context: &TransactionContext,
    workspace_id: &str,
    revision: Revision,
    payload: Value,
) -> Result<EventDraft, GraphTransactionError> {
    event(
        context,
        "work_graph",
        workspace_id.to_string(),
        revision.get(),
        WORK_PLAN_APPLIED,
        workspace_id,
        None,
        payload,
    )
}

fn edge_draft(
    context: &TransactionContext,
    edge: &WorkEdge,
    event_type: &str,
    version: u64,
) -> Result<EventDraft, GraphTransactionError> {
    event(
        context,
        "work_edge",
        edge.id.to_string(),
        version,
        event_type,
        &edge.workspace_id,
        None,
        json!({
            "edge_id": edge.id.to_string(),
            "workspace_id": edge.workspace_id,
            "from_node_id": edge.from_node_id.to_string(),
            "to_node_id": edge.to_node_id.to_string(),
            "kind": edge.kind.as_db_str(),
            "revision": edge.revision.get(),
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn event(
    context: &TransactionContext,
    aggregate_type: &str,
    aggregate_id: String,
    aggregate_version: u64,
    event_type: &str,
    workspace_id: &str,
    generation: Option<Generation>,
    payload: Value,
) -> Result<EventDraft, GraphTransactionError> {
    let event_type = EventType::parse(event_type)?;
    let mut draft = EventDraft::new(
        aggregate_type,
        aggregate_id,
        aggregate_version,
        event_type,
        context.correlation_id,
        context.actor.clone(),
    )
    .with_workspace(workspace_id)
    .with_payload(payload);
    if let Some(generation) = generation {
        draft = draft.with_generation(generation);
    }
    if let Some(command_id) = context.command_id {
        draft = draft.with_command_id(command_id);
    }
    if let Some(causation_id) = &context.causation_id {
        draft = draft.with_causation_id(causation_id.clone());
    }
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quansio_core::{CanonicalId, Prefix};
    use serde_json::json;

    use crate::runtime::NewRun;
    use crate::state::{RunTriggerKind, WorkNodeKind};
    use crate::work::NewWorkNode;

    #[test]
    fn agent_thread_targets_use_domain_names_and_refuse_the_un_named_states() {
        assert_eq!(
            agent_thread_event_name(AgentThreadStatus::Active),
            Some("agent.activated")
        );
        assert_eq!(
            agent_thread_event_name(AgentThreadStatus::HandingOff),
            Some("agent.handoff_started")
        );
        assert_eq!(
            agent_thread_event_name(AgentThreadStatus::HandedOff),
            Some("agent.handoff_completed")
        );
        assert_eq!(
            agent_thread_event_name(AgentThreadStatus::Joined),
            Some("agent.joined")
        );
        assert_eq!(
            agent_thread_event_name(AgentThreadStatus::Terminated),
            Some("agent.terminated")
        );
        assert_eq!(
            agent_thread_event_name(AgentThreadStatus::Provisioned),
            None
        );
        assert_eq!(agent_thread_event_name(AgentThreadStatus::Joining), None);
    }

    #[test]
    fn run_targets_use_domain_names() {
        for (from, to, expected) in [
            (RunStatus::Created, RunStatus::Queued, "run.queued"),
            (RunStatus::Queued, RunStatus::Running, "run.started"),
            (RunStatus::WaitingChild, RunStatus::Running, "run.resumed"),
            (RunStatus::Suspended, RunStatus::Running, "run.resumed"),
            (
                RunStatus::Running,
                RunStatus::WaitingApproval,
                "run.waiting",
            ),
            (RunStatus::Running, RunStatus::Verifying, "run.verifying"),
            (RunStatus::Verifying, RunStatus::Succeeded, "run.succeeded"),
            (RunStatus::Verifying, RunStatus::Failed, "run.failed"),
            (RunStatus::Queued, RunStatus::Cancelled, "run.cancelled"),
            (
                RunStatus::Running,
                RunStatus::BlockedUnrecoverable,
                "run.blocked",
            ),
            (RunStatus::Running, RunStatus::Suspended, "run.suspended"),
        ] {
            assert_eq!(
                run_event_name(from, to),
                Some(expected),
                "{from:?} -> {to:?}"
            );
        }
        assert_eq!(run_event_name(RunStatus::Created, RunStatus::Created), None);
    }

    #[test]
    fn every_named_event_type_is_a_canonical_family_member() {
        for name in [
            WORK_NODE_CREATED,
            WORK_NODE_STATUS_CHANGED,
            WORK_EDGE_ADDED,
            WORK_EDGE_REMOVED,
            WORK_PLAN_APPLIED,
            AGENT_THREAD_PROVISIONED,
            AGENT_DELEGATED,
            RUN_CREATED,
            "agent.activated",
            "agent.suspended",
            "agent.handoff_started",
            "agent.handoff_completed",
            "agent.joined",
            "agent.terminated",
            "run.queued",
            "run.started",
            "run.resumed",
            "run.waiting",
            "run.verifying",
            "run.succeeded",
            "run.failed",
            "run.cancelled",
            "run.blocked",
            "run.suspended",
        ] {
            let parsed = EventType::parse(name).expect("canonical event type");
            assert_eq!(parsed.to_string(), name);
        }
    }

    #[test]
    fn unmapped_change_kinds_are_refused() {
        let error = check_mappable(&GraphChange::StartAttempt {
            step_id: quansio_core::CanonicalId::parse_typed(
                "stp_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
                quansio_core::Prefix::Step,
            )
            .expect("step id"),
            generation: Generation::INITIAL,
        })
        .expect_err("attempts have no DOMAIN event name yet");
        assert_eq!(error.code(), "VALIDATION_SCHEMA");
        assert!(
            matches!(error, GraphTransactionError::UnsupportedChange { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn mapped_change_kinds_are_accepted() {
        let node_id = CanonicalId::parse_typed("wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", Prefix::WorkNode)
            .expect("node id");
        let thread_id =
            CanonicalId::parse_typed("ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", Prefix::AgentThread)
                .expect("thread id");
        for change in [
            GraphChange::create_work_node(NewWorkNode::new(
                "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
                WorkNodeKind::Task,
                "planned",
                json!({"kind": "agent", "id": "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"}),
            )),
            GraphChange::create_run(NewRun::new(
                "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
                node_id,
                thread_id,
                RunTriggerKind::Manual,
            )),
            GraphChange::TransitionRun {
                run_id: CanonicalId::parse_typed("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", Prefix::Run)
                    .expect("run id"),
                to: RunStatus::Running,
                terminal_reason: None,
            },
        ] {
            check_mappable(&change).expect("mapped change kind");
        }
    }
}
