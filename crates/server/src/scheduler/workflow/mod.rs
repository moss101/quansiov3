//! Workflow scripts: the executor (ADP-007, DOMAIN.md §13.8 E to H).
//!
//! A script run is an Objective, a Run of the routine's teammate that makes no model call of its own, and one worker Run per
//! call. This module drives it: from the graph and the journal alone it works out what is done, what is in flight and what comes
//! next, so a restart, a crash between any two writes and a second instance all resume the same run and repeat no finished call.
//!
//! * [`journal`] — the append-only record, kept as Evidence on the script run;
//! * `graph` — nodes and edges through the RUN-003 planner, status changes through `GraphTransaction`;
//! * [`gate`] — the approvals (fan-out above the call threshold, extending the call budget) through the ordinary approval path;
//! * `drive` — the executor itself.
//!
//! Running one worker call (its thread, its message, its Run, its turn, its reply) is [`WorkflowPort`], implemented by the
//! deployable over the same engine an interactive message uses, so a worker's tool calls take the ordinary policy, capability
//! and Effect Ledger path.

mod drive;
pub mod gate;
mod graph;
pub mod journal;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::control::routines::config::AutomationConfig;
use crate::control::routines::script::Effort;
use crate::control::schema::SchemaError;

pub use drive::{overview_of, DriveReport, DriveStatus};
pub use gate::is_gate_effect;

/// Why a script operation failed. A failure the next pass can retry leaves the run as it was.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// PostgreSQL rejected a statement.
    #[error("workflow database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant context could not be set.
    #[error("workflow schema: {0}")]
    Schema(#[from] SchemaError),
    /// The journal could not be read or written.
    #[error("workflow journal: {0}")]
    Journal(String),
    /// The WorkGraph refused.
    #[error("workflow graph: {0}")]
    Graph(String),
    /// The planner refused a proposal.
    #[error("workflow plan: {0}")]
    Plan(String),
    /// The approval path refused.
    #[error("workflow approval: {0}")]
    Policy(String),
    /// The Effect Ledger refused.
    #[error("workflow effect: {0}")]
    Effect(String),
    /// The runtime refused a run transition.
    #[error("workflow runtime: {0}")]
    Runtime(String),
    /// The script itself is not valid (it was valid when it was saved).
    #[error("workflow script: {0}")]
    Script(String),
    /// The worker port failed.
    #[error("workflow call: {0}")]
    Port(String),
}

/// What a script run needs to know about itself, kept in its journal's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRef {
    /// Tenant.
    pub tenant_id: String,
    /// Workspace.
    pub workspace_id: String,
    /// The routine.
    pub routine_id: String,
    /// The routine's owner (notified).
    pub owner_user_id: String,
    /// The routine's title.
    pub title: String,
    /// The fire this run executes.
    pub fire_key: String,
    /// The script run (`run_…`).
    pub run_id: String,
    /// The Objective node.
    pub objective_id: String,
    /// The script run's AgentThread (the teammate's).
    pub agent_thread_id: String,
    /// The teammate definition the routine names, when it does.
    pub teammate_id: Option<String>,
}

impl ScriptRef {
    /// The journal's spelling.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "tenant_id": self.tenant_id, "workspace_id": self.workspace_id, "routine_id": self.routine_id,
            "owner_user_id": self.owner_user_id, "title": self.title, "fire_key": self.fire_key,
            "run_id": self.run_id, "objective_id": self.objective_id, "agent_thread_id": self.agent_thread_id,
            "teammate_id": self.teammate_id,
        })
    }

    /// Read it back.
    ///
    /// # Errors
    /// [`WorkflowError::Journal`] for a head that lacks a field.
    pub fn from_json(value: &Value) -> Result<Self, WorkflowError> {
        let text = |key: &str| -> Result<String, WorkflowError> {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| WorkflowError::Journal(format!("the journal head has no {key}")))
        };
        Ok(Self {
            tenant_id: text("tenant_id")?,
            workspace_id: text("workspace_id")?,
            routine_id: text("routine_id")?,
            owner_user_id: text("owner_user_id")?,
            title: text("title")?,
            fire_key: text("fire_key")?,
            run_id: text("run_id")?,
            objective_id: text("objective_id")?,
            agent_thread_id: text("agent_thread_id")?,
            teammate_id: value
                .get("teammate_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

/// One worker call to run.
#[derive(Debug, Clone)]
pub struct AgentCall {
    /// The script run this call belongs to.
    pub script: ScriptRef,
    /// The call's WorkNode.
    pub node_id: String,
    /// The call's key (`step` or `step#index`): the identity every lookup of its thread, message and Run is made by.
    pub key: String,
    /// The message the worker is given, as content blocks: the author's words as `text`, every substituted value as
    /// `untrusted_data`, then the reply contract.
    pub blocks: Vec<Value>,
    /// The route class.
    pub effort: Effort,
}

/// How far a call got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallProgress {
    /// The worker's Run is parked (an approval, a question) or still working: look again on a later pass.
    Pending {
        /// The worker's Run.
        run_id: Option<String>,
        /// Why.
        reason: String,
    },
    /// The worker finished its turn.
    Replied {
        /// The worker's Run.
        run_id: String,
        /// What it said.
        text: String,
    },
    /// The worker's Run ended without a reply (failed, cancelled, refused).
    Failed {
        /// The worker's Run, when one was made.
        run_id: Option<String>,
        /// A code.
        code: String,
        /// What happened.
        message: String,
    },
}

/// Runs one worker call. Idempotent per [`AgentCall::key`]: a call that was started continues from where it stopped and never
/// starts a second worker.
#[async_trait]
pub trait WorkflowPort: Send + Sync {
    /// Run the call to its reply, or report that it is still in flight.
    ///
    /// # Errors
    /// [`WorkflowError::Port`] for a failure the next pass may retry.
    async fn run_call(&self, call: AgentCall) -> Result<CallProgress, WorkflowError>;
}

/// The executor of script runs.
#[derive(Clone)]
pub struct WorkflowExecutor {
    pool: PgPool,
    config: Arc<AutomationConfig>,
    port: Arc<dyn WorkflowPort>,
}

impl WorkflowExecutor {
    /// An executor over the pool running calls through `port`.
    #[must_use]
    pub fn new(pool: PgPool, config: Arc<AutomationConfig>, port: Arc<dyn WorkflowPort>) -> Self {
        Self { pool, config, port }
    }
}
