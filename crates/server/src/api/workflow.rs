//! The deployable's worker port for workflow scripts (ADP-007, DOMAIN.md §13.8 H).
//!
//! One call is a worker: an AgentThread delegated from the script run's teammate (capability only narrows), its own thread so no
//! item sees another's context, one message carrying the step's instruction, one Run, one turn through the same engine an
//! interactive message uses, and the reply read back from the thread. Every lookup is made by the call's node before anything is
//! created, so a call that was started continues from where it stopped and never starts a second worker.

use std::sync::Arc;

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use serde_json::{json, Value};
use sqlx::Row;

use super::commands::{
    chat_engine, internal, load_target, post_agent_reply, runtime_error, seed_projection,
};
use super::routines::existing_run;
use super::{ApiError, ApiState};
use crate::control::collaboration::CollaborationStore;
use crate::control::conversation::{ConversationStore, NewMessage};
use crate::runtime::agents::{
    AgentKind, AgentStore, NewAgentThread, NewDelegation, StructuralDelegationCheck,
};
use crate::runtime::state_machine::{
    NewRun, RunStatus, RunTriggerKind, RuntimeIdentity, TurnInput, TurnStatus,
};
use crate::scheduler::workflow::{
    AgentCall, CallProgress, WorkflowError, WorkflowExecutor, WorkflowPort,
};

/// The executor the deployable runs script runs with: the shipped automation policy and this API state's worker port.
#[must_use]
pub fn executor(state: &ApiState) -> WorkflowExecutor {
    WorkflowExecutor::new(
        state.pool().clone(),
        Arc::clone(state.automation()),
        Arc::new(ApiWorkflowPort::new(state.clone())),
    )
}

/// Runs workflow calls on the interactive path.
#[derive(Clone)]
pub struct ApiWorkflowPort {
    state: ApiState,
}

impl ApiWorkflowPort {
    /// A port over the composed API state.
    #[must_use]
    pub fn new(state: ApiState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl WorkflowPort for ApiWorkflowPort {
    async fn run_call(&self, call: AgentCall) -> Result<CallProgress, WorkflowError> {
        run_call(&self.state, &call)
            .await
            .map_err(|error| WorkflowError::Port(format!("{}: {}", error.code, error.message)))
    }
}

async fn run_call(state: &ApiState, call: &AgentCall) -> Result<CallProgress, ApiError> {
    let script = &call.script;
    let correlation_id = CorrelationId::generate(&mut UlidGenerator::new());
    let correlation = correlation_id.to_string();
    let identity = RuntimeIdentity::system(
        &script.tenant_id,
        format!("workflow:{}", script.run_id),
        correlation_id,
    );
    let node_id = CanonicalId::parse_typed(&call.node_id, Prefix::WorkNode)
        .map_err(|error| internal(error, &correlation))?;

    // 1. The call's own thread, found by its title (the script run and the call's key), else created.
    let title = format!("{} [{}] {}", script.title, call.key, script.run_id);
    let thread_id = match thread_titled(
        state,
        &script.tenant_id,
        &script.workspace_id,
        &title,
        &correlation,
    )
    .await?
    {
        Some(thread) => thread,
        None => {
            CollaborationStore::new(state.pool().clone(), identity.clone())
                .create_thread(
                    &script.workspace_id,
                    "objective",
                    Some(title),
                    None,
                    Vec::new(),
                )
                .await
                .map_err(|error| super::collaboration::collaboration_error(error, &correlation))?
                .0
                .id
        }
    };

    // 2. The step's message, found by its notice, else posted. Author `routine:<id>`, like the fire's own message, so the event
    //    triggers' guard against a routine's own activity covers it.
    let conversation = ConversationStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, &correlation))?;
    let marker = json!([{ "type": "system_notice", "notice": {
        "workflow_call": call.key, "script_run": script.run_id } }]);
    let message = match conversation
        .find_by_block(&thread_id, &marker)
        .await
        .map_err(|error| internal(error, &correlation))?
    {
        Some(message) => message,
        None => {
            let new = NewMessage {
                author_kind: "system",
                author_id: format!("routine:{}", script.routine_id),
                content_blocks: Value::Array(call.blocks.clone()),
                attachments: Vec::new(),
                mentions: Vec::new(),
                run_id: None,
                turn_id: None,
            };
            CollaborationStore::new(state.pool().clone(), identity.clone())
                .post(&thread_id, new, None)
                .await
                .map_err(|error| super::collaboration::collaboration_error(error, &correlation))?
                .1
        }
    };

    // 3. The worker, found by its node, else delegated from the script run's teammate thread and activated.
    let agents = AgentStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| runtime_error(error, &correlation))?;
    let worker = match worker_of(state, &script.tenant_id, &call.node_id, &correlation).await? {
        Some(worker) => worker,
        None => {
            let parent = CanonicalId::parse_typed(&script.agent_thread_id, Prefix::AgentThread)
                .map_err(|error| internal(error, &correlation))?;
            let child = NewAgentThread::new(&script.workspace_id, AgentKind::Worker)
                .with_work_node(node_id);
            let delegated = agents
                .delegate_with(
                    Arc::new(StructuralDelegationCheck),
                    &parent,
                    NewDelegation::new(child)
                        .with_instruction(format!("workflow step {}", call.key))
                        .with_work_node(node_id),
                )
                .await
                .map_err(|error| runtime_error(error, &correlation))?;
            agents
                .activate(&delegated.child.id, delegated.child.generation)
                .await
                .map_err(|error| runtime_error(error, &correlation))?
                .id
        }
    };

    // 4. The worker's Run, found by its message, else created, projected and started.
    let engine =
        chat_engine(state, identity.clone()).map_err(|error| internal(error, &correlation))?;
    let existing = existing_run(state, &script.tenant_id, &message.id, &correlation).await?;
    let mut run = match existing {
        Some(run_id) => engine
            .store()
            .load_run(&run_id)
            .await
            .map_err(|error| runtime_error(error, &correlation))?,
        None => {
            let mut new_run = NewRun::new(
                script.workspace_id.clone(),
                node_id,
                worker,
                RunTriggerKind::Routine,
            );
            new_run.trigger_ref = Some(message.id.clone());
            new_run.budget_snapshot = json!({ "effort": call.effort.as_str() });
            if let Some(target) =
                load_target(state, &script.tenant_id, &script.workspace_id).await?
            {
                new_run.execution_target_id = Some(target);
            }
            let run = engine
                .create_run(new_run)
                .await
                .map_err(|error| runtime_error(error, &correlation))?;
            seed_projection(state, &identity, &script.workspace_id, &run)
                .await
                .map_err(|error| internal(error, &correlation))?;
            run
        }
    };
    if run.status == RunStatus::Created {
        run = engine
            .enqueue(&run.id, run.generation)
            .await
            .map_err(|error| runtime_error(error, &correlation))?;
    }
    if run.status == RunStatus::Queued {
        run = engine
            .start(&run.id, run.generation)
            .await
            .map_err(|error| runtime_error(error, &correlation))?;
    }

    // 5. The turn: the worker's one model-driven loop, unless it already ran. A turn a crash cut short is recovered, and a turn
    //    that was aborted is taken again.
    for _ in 0..2 {
        let turns = engine
            .store()
            .list_turns(&run.id)
            .await
            .map_err(|error| runtime_error(error, &correlation))?;
        let last = turns.last().map(|turn| turn.status);
        match last {
            Some(TurnStatus::Active) => {
                engine
                    .recover(&run.id)
                    .await
                    .map_err(|error| runtime_error(error, &correlation))?;
                run = engine
                    .store()
                    .load_run(&run.id)
                    .await
                    .map_err(|error| runtime_error(error, &correlation))?;
            }
            None | Some(TurnStatus::Aborted) if run.status == RunStatus::Running => {
                let outcome = engine
                    .run_turn(
                        &run.id,
                        run.generation,
                        TurnInput::new(RunTriggerKind::Routine).with_reference(message.id.clone()),
                    )
                    .await
                    .map_err(|error| runtime_error(error, &correlation))?;
                post_agent_reply(
                    &conversation,
                    &thread_id,
                    &worker.to_string(),
                    &run.id.to_string(),
                    &outcome,
                    &correlation,
                )
                .await?;
                run = engine
                    .store()
                    .load_run(&run.id)
                    .await
                    .map_err(|error| runtime_error(error, &correlation))?;
            }
            _ => break,
        }
    }
    let run_id = run.id.to_string();

    // 6. Where the call stands.
    match run.status {
        RunStatus::Failed | RunStatus::Cancelled | RunStatus::BlockedUnrecoverable => {
            return Ok(CallProgress::Failed {
                run_id: Some(run_id),
                code: run
                    .terminal_reason
                    .clone()
                    .unwrap_or_else(|| run.status.as_db_str().to_string()),
                message: format!("the worker's run is {}", run.status.as_db_str()),
            });
        }
        RunStatus::Succeeded | RunStatus::Running => {}
        other => {
            return Ok(CallProgress::Pending {
                run_id: Some(run_id),
                reason: format!("the worker's run is {}", other.as_db_str()),
            });
        }
    }
    let turns = engine
        .store()
        .list_turns(&run.id)
        .await
        .map_err(|error| runtime_error(error, &correlation))?;
    if turns.last().map(|turn| turn.status) != Some(TurnStatus::Completed)
        && run.status == RunStatus::Running
    {
        return Ok(CallProgress::Pending {
            run_id: Some(run_id),
            reason: "the worker's turn has not completed".to_string(),
        });
    }
    match reply_of(state, &script.tenant_id, &thread_id, &run_id, &correlation).await? {
        Some(text) => {
            retire(&agents, &worker).await;
            Ok(CallProgress::Replied { run_id, text })
        }
        None => Ok(CallProgress::Failed {
            run_id: Some(run_id),
            code: "NO_REPLY".to_string(),
            message: "the worker finished its turn without a reply".to_string(),
        }),
    }
}

/// Join the worker to its parent and end it. A worker another path already ended is not an error.
async fn retire(agents: &AgentStore, worker: &CanonicalId) {
    if let Ok(thread) = agents.load_thread(worker).await {
        let _ = agents
            .join_worker(crate::runtime::agents::JoinRequest::new(
                *worker,
                thread.generation,
            ))
            .await;
    }
    if let Ok(thread) = agents.load_thread(worker).await {
        let _ = agents.terminate(worker, thread.generation).await;
    }
}

async fn thread_titled(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
    title: &str,
    correlation: &str,
) -> Result<Option<String>, ApiError> {
    let mut tx = state.tenant_tx(tenant_id, correlation).await?;
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM threads WHERE tenant_id = $1 AND workspace_id = $2 AND title = $3 \
         ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(title)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| internal(error, correlation))?;
    tx.commit()
        .await
        .map_err(|error| internal(error, correlation))?;
    Ok(id)
}

async fn worker_of(
    state: &ApiState,
    tenant_id: &str,
    node_id: &str,
    correlation: &str,
) -> Result<Option<CanonicalId>, ApiError> {
    let mut tx = state.tenant_tx(tenant_id, correlation).await?;
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM agent_threads WHERE tenant_id = $1 AND work_node_id = $2 AND agent_kind = 'worker' \
         ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| internal(error, correlation))?;
    tx.commit()
        .await
        .map_err(|error| internal(error, correlation))?;
    id.map(|id| {
        CanonicalId::parse_typed(&id, Prefix::AgentThread)
            .map_err(|error| internal(error, correlation))
    })
    .transpose()
}

/// What the worker said: the text of its latest message in the call's thread.
async fn reply_of(
    state: &ApiState,
    tenant_id: &str,
    thread_id: &str,
    run_id: &str,
    correlation: &str,
) -> Result<Option<String>, ApiError> {
    let mut tx = state.tenant_tx(tenant_id, correlation).await?;
    let row = sqlx::query(
        "SELECT content_blocks FROM messages WHERE tenant_id = $1 AND thread_id = $2 \
         AND author_kind = 'agent' AND run_id = $3 ORDER BY seq DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| internal(error, correlation))?;
    tx.commit()
        .await
        .map_err(|error| internal(error, correlation))?;
    Ok(row
        .map(|row| {
            let blocks: Value = row.get("content_blocks");
            blocks
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        })
        .filter(|text| !text.trim().is_empty()))
}
