//! The turn loop's [`DelegationPort`] backed by the real AgentThread store (RUN-002).
//!
//! DOMAIN.md §5.6 routes a proposed `delegate_request` to RUN-002: this port creates a
//! real child AgentThread under the proposing run's thread with a capability projection
//! narrowed from the parent, delivers the instruction to the child's mailbox, and
//! returns [`DelegationOutcome::Spawned`]. The engine then parks the run in
//! `WAITING_CHILD`; the port never advances the turn loop itself.

use std::sync::Arc;

use async_trait::async_trait;
use quansio_core::{CanonicalId, Generation, Prefix};

use super::super::state_machine::{
    DelegationContext, DelegationOutcome, DelegationPort, DelegationRequest, RunStatus,
    RuntimeError, RuntimeIdentity, RuntimeStore,
};
use super::store::{
    AgentStore, DelegationNarrowingCheck, NewAgentThread, NewDelegation, StructuralDelegationCheck,
};
use super::{fence, AgentKind, AgentThreadStatus};

/// The real delegation seam: a proposed delegation becomes a durable child AgentThread.
pub struct AgentDelegationPort {
    agents: AgentStore,
    runs: RuntimeStore,
    check: Arc<dyn DelegationNarrowingCheck>,
}

impl AgentDelegationPort {
    /// Build the port with an explicit narrowing check (the RUN-005 seam).
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when `identity.tenant_id` is not canonical.
    pub fn new(
        pool: sqlx::PgPool,
        identity: RuntimeIdentity,
        check: Arc<dyn DelegationNarrowingCheck>,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            agents: AgentStore::new(pool.clone(), identity.clone())?,
            runs: RuntimeStore::new(pool, identity)?,
            check,
        })
    }

    /// Build the port with the structural narrowing rule shipped before RUN-005.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when `identity.tenant_id` is not canonical.
    pub fn with_structural_check(
        pool: sqlx::PgPool,
        identity: RuntimeIdentity,
    ) -> Result<Self, RuntimeError> {
        Self::new(pool, identity, Arc::new(StructuralDelegationCheck))
    }

    /// The AgentThread store this port mutates.
    #[must_use]
    pub fn agents(&self) -> &AgentStore {
        &self.agents
    }
}

#[async_trait]
impl DelegationPort for AgentDelegationPort {
    async fn delegate(
        &self,
        request: DelegationRequest,
        context: DelegationContext,
    ) -> Result<DelegationOutcome, RuntimeError> {
        let run_id = CanonicalId::parse_typed(&context.run_id, Prefix::Run)?;
        let generation = Generation::new(context.generation)
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let run = self.runs.load_run(&run_id).await?;
        fence(run.generation, generation)?;
        if run.status != RunStatus::Running {
            return Err(RuntimeError::IllegalTransition {
                entity: "run",
                from: run.status.as_db_str().to_string(),
                to: "delegation".to_string(),
            });
        }
        let parent = self.agents.load_thread(&run.agent_thread_id).await?;
        if parent.status != AgentThreadStatus::Active {
            return Err(RuntimeError::IllegalTransition {
                entity: "agent_thread",
                from: parent.status.as_db_str().to_string(),
                to: "delegation".to_string(),
            });
        }
        let work_node_id = match &request.work_node_id {
            Some(node) => CanonicalId::parse_typed(node, Prefix::WorkNode)?,
            None => run.work_node_id,
        };
        let mut child =
            NewAgentThread::new(&run.workspace_id, AgentKind::Worker).with_work_node(work_node_id);
        if let Some(projection) = parent.capability_projection_id {
            child = child.with_capability_projection(projection);
        }
        let delegation = self
            .agents
            .delegate_with(
                Arc::clone(&self.check),
                &parent.id,
                NewDelegation::new(child)
                    .with_instruction(request.instruction)
                    .with_work_node(work_node_id),
            )
            .await?;
        let child = self
            .agents
            .activate(&delegation.child.id, delegation.child.generation)
            .await?;
        Ok(DelegationOutcome::Spawned {
            child_agent_thread_id: child.id.to_string(),
            work_node_id: child.work_node_id.map(|node| node.to_string()),
        })
    }
}
