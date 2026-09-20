//! The graph-backed [`RunDispatch`] (CORE-008 over CORE-005): scheduler-driven work moves
//! through the canonical Run state machine and the canonical Objective/Run creation path
//! in the graph store, never through a second state machine.

use quansio_core::CanonicalId;
use quansio_graph::{
    AgentKind, GraphError, GraphStore, NewAgentThread, NewRun, NewWorkNode, RunStatus,
    RunTriggerKind, WorkNodeKind, WorkOrigin,
};
use serde_json::json;
use sqlx::PgPool;

use super::{
    DispatchOutcome, RoutineFireOutcome, RoutineFireRequest, RunDispatch, RunDispatchRequest,
    RunResumeRequest, SchedulerError,
};

/// The production dispatch port: the only implementation that touches Run/WorkGraph state.
pub struct GraphDispatch {
    pool: PgPool,
    tenant_id: String,
}

impl GraphDispatch {
    /// A port over the pool for one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.into(),
        }
    }

    fn store(&self, subject: &str) -> Result<GraphStore, SchedulerError> {
        GraphStore::new(self.pool.clone(), self.tenant_id.clone())
            .map_err(|error| failure(subject, error))
    }
}

fn failure(run_id: &str, error: GraphError) -> SchedulerError {
    SchedulerError::RunDispatch {
        run_id: run_id.to_string(),
        reason: error.to_string(),
    }
}

fn run_id(value: &str) -> Result<CanonicalId, SchedulerError> {
    CanonicalId::parse_typed(value, quansio_core::Prefix::Run).map_err(|error| {
        SchedulerError::RunDispatch {
            run_id: value.to_string(),
            reason: error.to_string(),
        }
    })
}

#[async_trait::async_trait]
impl RunDispatch for GraphDispatch {
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        let store = self.store(&request.run_id)?;
        let run_id = run_id(&request.run_id)?;
        match store
            .transition_run(&run_id, RunStatus::Running, None)
            .await
        {
            Ok(_) => Ok(DispatchOutcome::Dispatched),
            Err(GraphError::IllegalTransition { .. }) => Ok(DispatchOutcome::NotDispatchable),
            Err(error) => Err(failure(&request.run_id, error)),
        }
    }

    async fn resume_timer_wait(
        &self,
        request: RunResumeRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        let store = self.store(&request.run_id)?;
        let run_id = run_id(&request.run_id)?;
        let run = store
            .get_run(&run_id)
            .await
            .map_err(|error| failure(&request.run_id, error))?;
        if run.generation.get() != request.generation {
            return Ok(DispatchOutcome::Fenced);
        }
        if run.status == RunStatus::Running {
            return Ok(DispatchOutcome::AlreadyRunning);
        }
        if !run.status.is_waiting() {
            return Ok(DispatchOutcome::NotDispatchable);
        }
        store
            .transition_run(&run_id, RunStatus::Running, None)
            .await
            .map_err(|error| failure(&request.run_id, error))?;
        Ok(DispatchOutcome::Dispatched)
    }

    async fn fire_routine(
        &self,
        request: RoutineFireRequest,
    ) -> Result<RoutineFireOutcome, SchedulerError> {
        let store = self.store(&request.routine_id)?;
        let title = request
            .objective_template
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Routine objective")
            .to_string();
        let mut node = NewWorkNode::new(
            &request.workspace_id,
            WorkNodeKind::Objective,
            title,
            json!({ "kind": "routine", "id": request.routine_id }),
        );
        node.origin = WorkOrigin::Routine;
        let objective = store
            .create_node(node)
            .await
            .map_err(|error| failure(&request.routine_id, error))?;
        let thread = store
            .create_agent_thread(NewAgentThread::new(
                &request.workspace_id,
                AgentKind::Teammate,
            ))
            .await
            .map_err(|error| failure(&request.routine_id, error))?;
        let mut run = NewRun::new(
            &request.workspace_id,
            objective.id,
            thread.id,
            RunTriggerKind::Routine,
        );
        run.trigger_ref = Some(request.routine_id.clone());
        let run = store
            .create_run(run)
            .await
            .map_err(|error| failure(&request.routine_id, error))?;
        // Enter the scheduler's dispatch path through the canonical state machine.
        store
            .transition_run(&run.id, RunStatus::Queued, None)
            .await
            .map_err(|error| failure(&run.id.to_string(), error))?;
        Ok(RoutineFireOutcome {
            objective_id: objective.id.to_string(),
            run_id: run.id.to_string(),
        })
    }
}
