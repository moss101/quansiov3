//! StateGraph store: `runs`, `turns`, `steps` and `attempts` (DOMAIN.md §5.2–§5.5).
//!
//! The Run state machine is applied exactly as specified; an illegal transition returns
//! [`GraphError::IllegalTransition`] whose [`GraphError::code`] is
//! `RUNTIME_ILLEGAL_TRANSITION`, and the row is left untouched. Attempts are append-only:
//! [`GraphStore::start_attempt`] always inserts the next `seq` for the step, and
//! [`GraphStore::finish_attempt`] only advances the attempt it names.

use quansio_core::{CanonicalId, Generation, Prefix};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::{Postgres, Row, Transaction};

use crate::error::{Entity, GraphError};
use crate::state::{AttemptStatus, RunStatus, RunTriggerKind, StepKind, StepStatus, TurnStatus};
use crate::store::GraphStore;

/// Fields required to create a [`Run`] (DOMAIN.md §5.2).
#[derive(Debug, Clone)]
pub struct NewRun {
    /// Workspace the run executes in.
    pub workspace_id: String,
    /// WorkNode being executed.
    pub work_node_id: CanonicalId,
    /// AgentThread executing the node.
    pub agent_thread_id: CanonicalId,
    /// What started the run.
    pub trigger_kind: RunTriggerKind,
    /// Trigger reference (message id, routine id, child run id, …).
    pub trigger_ref: Option<String>,
    /// Budget snapshot taken at creation.
    pub budget_snapshot: Value,
    /// Execution target bound to the run, when already known.
    pub execution_target_id: Option<CanonicalId>,
}

impl NewRun {
    /// A run with an empty budget snapshot.
    #[must_use]
    pub fn new(
        workspace_id: impl Into<String>,
        work_node_id: CanonicalId,
        agent_thread_id: CanonicalId,
        trigger_kind: RunTriggerKind,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            work_node_id,
            agent_thread_id,
            trigger_kind,
            trigger_ref: None,
            budget_snapshot: json!({}),
            execution_target_id: None,
        }
    }
}

/// A persisted `runs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Canonical `run_` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// WorkNode being executed.
    pub work_node_id: CanonicalId,
    /// AgentThread executing the node.
    pub agent_thread_id: CanonicalId,
    /// Fencing generation.
    pub generation: Generation,
    /// Current state.
    pub status: RunStatus,
    /// What started the run.
    pub trigger_kind: RunTriggerKind,
    /// Trigger reference.
    pub trigger_ref: Option<String>,
    /// Current turn.
    pub current_turn_id: Option<CanonicalId>,
    /// Budget snapshot.
    pub budget_snapshot: Value,
    /// Typed terminal reason.
    pub terminal_reason: Option<String>,
    /// Execution target.
    pub execution_target_id: Option<CanonicalId>,
    /// When the run started executing.
    pub started_at: Option<DateTime<Utc>>,
    /// When the run reached a terminal state.
    pub ended_at: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// Fields required to create a [`Turn`] (DOMAIN.md §5.3).
#[derive(Debug, Clone)]
pub struct NewTurn {
    /// Input kind (message, wake, child result, approval, answer, …).
    pub input_kind: String,
    /// Input reference.
    pub input_ref: Option<String>,
    /// ContextProjection built for this turn (`ctx_<ULID>`; not in the DOMAIN §1.1
    /// prefix table, so it is carried opaquely).
    pub context_projection_id: Option<String>,
}

impl NewTurn {
    /// A turn with the given input kind.
    #[must_use]
    pub fn new(input_kind: impl Into<String>) -> Self {
        Self {
            input_kind: input_kind.into(),
            input_ref: None,
            context_projection_id: None,
        }
    }
}

/// A persisted `turns` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// Canonical `trn_` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Parent run.
    pub run_id: CanonicalId,
    /// Run-monotonic sequence.
    pub seq: i32,
    /// Input kind.
    pub input_kind: String,
    /// Input reference.
    pub input_ref: Option<String>,
    /// ContextProjection id.
    pub context_projection_id: Option<String>,
    /// Current state.
    pub status: TurnStatus,
    /// Number of steps emitted by the turn loop.
    pub step_count: i32,
    /// Token ledger.
    pub token_ledger: Value,
    /// When the turn started.
    pub started_at: DateTime<Utc>,
    /// When the turn finished.
    pub ended_at: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// Fields required to create a [`Step`] (DOMAIN.md §5.4).
#[derive(Debug, Clone)]
pub struct NewStep {
    /// Step kind.
    pub kind: StepKind,
    /// Model call id, tool call id, child agent thread id or wait id.
    pub step_ref: Option<String>,
    /// EffectRecord reserved by this step.
    pub effect_id: Option<String>,
}

impl NewStep {
    /// A step with the given kind.
    #[must_use]
    pub fn new(kind: StepKind) -> Self {
        Self {
            kind,
            step_ref: None,
            effect_id: None,
        }
    }
}

/// A persisted `steps` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// Canonical `stp_` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Parent turn.
    pub turn_id: CanonicalId,
    /// Turn-monotonic sequence.
    pub seq: i32,
    /// Step kind.
    pub kind: StepKind,
    /// Current state.
    pub status: StepStatus,
    /// Dispatch reference.
    pub step_ref: Option<String>,
    /// Evidence ids.
    pub evidence_ids: Value,
    /// EffectRecord id.
    pub effect_id: Option<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// A persisted `attempts` row (DOMAIN.md §5.5).
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    /// Canonical `att_` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Parent step.
    pub step_id: CanonicalId,
    /// Step-monotonic sequence; a retry appends the next value and never reuses one.
    pub seq: i32,
    /// Generation the attempt was dispatched under.
    pub generation: Generation,
    /// Current state.
    pub status: AttemptStatus,
    /// Typed error.
    pub error: Option<Value>,
    /// When the attempt was dispatched.
    pub dispatched_at: DateTime<Utc>,
    /// When the attempt finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

const RUN_COLUMNS: &str =
    "id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, \
     status, trigger_kind, trigger_ref, current_turn_id, budget_snapshot, terminal_reason, \
     execution_target_id, started_at, ended_at, created_at, updated_at";

const TURN_COLUMNS: &str = "id, tenant_id, run_id, seq, input_kind, input_ref, \
     context_projection_id, status, step_count, token_ledger, started_at, ended_at, created_at, \
     updated_at";

const STEP_COLUMNS: &str = "id, tenant_id, turn_id, seq, kind, status, ref, evidence_ids, \
     effect_id, created_at, updated_at";

const ATTEMPT_COLUMNS: &str = "id, tenant_id, step_id, seq, generation, status, error, \
     dispatched_at, finished_at, created_at, updated_at";

fn generation_from_i64(value: i64) -> Result<Generation, GraphError> {
    Generation::new(value as u64)
        .map_err(|_| GraphError::InvalidArgument(format!("invalid generation {value}")))
}

fn run_from_row(row: &PgRow) -> Result<Run, GraphError> {
    Ok(Run {
        id: parse_id(row, "id", Prefix::Run)?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        work_node_id: parse_id(row, "work_node_id", Prefix::WorkNode)?,
        agent_thread_id: parse_id(row, "agent_thread_id", Prefix::AgentThread)?,
        generation: generation_from_i64(row.try_get("generation")?)?,
        status: RunStatus::from_db_str(row.try_get("status")?)?,
        trigger_kind: RunTriggerKind::from_db_str(row.try_get("trigger_kind")?)?,
        trigger_ref: row.try_get("trigger_ref")?,
        current_turn_id: GraphStore::optional_id(row.try_get("current_turn_id")?, Prefix::Turn)?,
        budget_snapshot: row.try_get("budget_snapshot")?,
        terminal_reason: row.try_get("terminal_reason")?,
        execution_target_id: GraphStore::optional_id(
            row.try_get("execution_target_id")?,
            Prefix::ExecutionTarget,
        )?,
        started_at: row.try_get("started_at")?,
        ended_at: row.try_get("ended_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn turn_from_row(row: &PgRow) -> Result<Turn, GraphError> {
    Ok(Turn {
        id: parse_id(row, "id", Prefix::Turn)?,
        tenant_id: row.try_get("tenant_id")?,
        run_id: parse_id(row, "run_id", Prefix::Run)?,
        seq: row.try_get("seq")?,
        input_kind: row.try_get("input_kind")?,
        input_ref: row.try_get("input_ref")?,
        context_projection_id: row.try_get("context_projection_id")?,
        status: TurnStatus::from_db_str(row.try_get("status")?)?,
        step_count: row.try_get("step_count")?,
        token_ledger: row.try_get("token_ledger")?,
        started_at: row.try_get("started_at")?,
        ended_at: row.try_get("ended_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn step_from_row(row: &PgRow) -> Result<Step, GraphError> {
    Ok(Step {
        id: parse_id(row, "id", Prefix::Step)?,
        tenant_id: row.try_get("tenant_id")?,
        turn_id: parse_id(row, "turn_id", Prefix::Turn)?,
        seq: row.try_get("seq")?,
        kind: StepKind::from_db_str(row.try_get("kind")?)?,
        status: StepStatus::from_db_str(row.try_get("status")?)?,
        step_ref: row.try_get("ref")?,
        evidence_ids: row.try_get("evidence_ids")?,
        effect_id: row.try_get("effect_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn attempt_from_row(row: &PgRow) -> Result<Attempt, GraphError> {
    Ok(Attempt {
        id: parse_id(row, "id", Prefix::Attempt)?,
        tenant_id: row.try_get("tenant_id")?,
        step_id: parse_id(row, "step_id", Prefix::Step)?,
        seq: row.try_get("seq")?,
        generation: generation_from_i64(row.try_get("generation")?)?,
        status: AttemptStatus::from_db_str(row.try_get("status")?)?,
        error: row.try_get("error")?,
        dispatched_at: row.try_get("dispatched_at")?,
        finished_at: row.try_get("finished_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn parse_id(row: &PgRow, column: &str, prefix: Prefix) -> Result<CanonicalId, GraphError> {
    let value: String = row.try_get(column)?;
    CanonicalId::parse_typed(&value, prefix).map_err(|_| GraphError::InvalidId {
        value,
        expected: prefix.as_str(),
    })
}

impl GraphStore {
    /// Create a run in `CREATED` state.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the workspace, WorkNode or AgentThread is not
    /// visible in this tenant/workspace.
    pub async fn create_run(&self, run: NewRun) -> Result<Run, GraphError> {
        let mut tx = self.begin().await?;
        let created = self.create_run_tx(&mut tx, run).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &created.workspace_id).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Read one run by id.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the run is not visible in this tenant.
    pub async fn get_run(&self, id: &CanonicalId) -> Result<Run, GraphError> {
        let mut tx = self.begin().await?;
        let run = self.get_run_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(run)
    }

    /// List a workspace's runs, newest first.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_runs(&self, workspace_id: &str) -> Result<Vec<Run>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE tenant_id = $1 AND workspace_id = $2 \
             ORDER BY created_at DESC, id"
        ))
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let runs = rows.iter().map(run_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(runs)
    }

    /// Apply a legal Run transition (DOMAIN.md §5.2).
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] (code `RUNTIME_ILLEGAL_TRANSITION`) when
    /// the transition is not in the state machine, and [`GraphError::StateConflict`] when
    /// the row changed concurrently. The row is unchanged on error.
    pub async fn transition_run(
        &self,
        id: &CanonicalId,
        to: RunStatus,
        terminal_reason: Option<String>,
    ) -> Result<Run, GraphError> {
        let mut tx = self.begin().await?;
        let updated = self
            .transition_run_tx(&mut tx, id, to, terminal_reason)
            .await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &updated.workspace_id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Create a turn at the next run sequence.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the run is not visible in this tenant.
    pub async fn create_turn(
        &self,
        run_id: &CanonicalId,
        turn: NewTurn,
    ) -> Result<Turn, GraphError> {
        let mut tx = self.begin().await?;
        let workspace_id = self.workspace_for_run_tx(&mut tx, run_id).await?;
        let created = self.create_turn_tx(&mut tx, run_id, turn).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &workspace_id).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Read one turn.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the turn is not visible in this tenant.
    pub async fn get_turn(&self, id: &CanonicalId) -> Result<Turn, GraphError> {
        let mut tx = self.begin().await?;
        let turn = self.get_turn_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(turn)
    }

    /// List a run's turns in sequence order.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_turns(&self, run_id: &CanonicalId) -> Result<Vec<Turn>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {TURN_COLUMNS} FROM turns WHERE tenant_id = $1 AND run_id = $2 ORDER BY seq"
        ))
        .bind(&self.tenant_id)
        .bind(run_id.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let turns = rows.iter().map(turn_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(turns)
    }

    /// Apply a legal Turn transition.
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] for an illegal transition and
    /// [`GraphError::StateConflict`] when the row changed concurrently.
    pub async fn transition_turn(
        &self,
        id: &CanonicalId,
        to: TurnStatus,
    ) -> Result<Turn, GraphError> {
        let mut tx = self.begin().await?;
        let updated = self.transition_turn_tx(&mut tx, id, to).await?;
        let workspace_id = self.workspace_for_run_tx(&mut tx, &updated.run_id).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &workspace_id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Apply a legal Turn transition inside an existing transaction.
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] for an illegal transition and
    /// [`GraphError::StateConflict`] when the row changed concurrently.
    pub(crate) async fn transition_turn_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        to: TurnStatus,
    ) -> Result<Turn, GraphError> {
        let current = self.lock_turn_tx(tx, id).await?;
        if !current.status.can_transition_to(to) {
            return Err(GraphError::IllegalTransition {
                entity: Entity::Turn,
                from: current.status.as_db_str().to_string(),
                to: to.as_db_str().to_string(),
            });
        }
        let updated = sqlx::query(
            "UPDATE turns SET status = $1, \
             ended_at = CASE WHEN $1 IN ('completed', 'aborted') THEN now() ELSE ended_at END \
             WHERE id = $2 AND tenant_id = $3 AND status = $4",
        )
        .bind(to.as_db_str())
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(current.status.as_db_str())
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(GraphError::StateConflict {
                entity: Entity::Turn.as_str(),
                id: id.to_string(),
                expected: current.status.as_db_str().to_string(),
            });
        }
        self.get_turn_tx(tx, id).await
    }

    /// Create a step at the next turn sequence.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the turn is not visible in this tenant.
    pub async fn create_step(
        &self,
        turn_id: &CanonicalId,
        step: NewStep,
    ) -> Result<Step, GraphError> {
        let mut tx = self.begin().await?;
        let created = self.create_step_tx(&mut tx, turn_id, step).await?;
        let workspace_id = self
            .workspace_for_turn_tx(&mut tx, &created.turn_id)
            .await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &workspace_id).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Read one step.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the step is not visible in this tenant.
    pub async fn get_step(&self, id: &CanonicalId) -> Result<Step, GraphError> {
        let mut tx = self.begin().await?;
        let step = self.get_step_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(step)
    }

    /// List a turn's steps in sequence order.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_steps(&self, turn_id: &CanonicalId) -> Result<Vec<Step>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {STEP_COLUMNS} FROM steps WHERE tenant_id = $1 AND turn_id = $2 ORDER BY seq"
        ))
        .bind(&self.tenant_id)
        .bind(turn_id.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let steps = rows.iter().map(step_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(steps)
    }

    /// Apply a legal Step transition.
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] (code `RUNTIME_ILLEGAL_TRANSITION`) for an
    /// illegal transition and [`GraphError::StateConflict`] when the row changed.
    pub async fn transition_step(
        &self,
        id: &CanonicalId,
        to: StepStatus,
    ) -> Result<Step, GraphError> {
        let mut tx = self.begin().await?;
        let updated = self.transition_step_tx(&mut tx, id, to).await?;
        let workspace_id = self
            .workspace_for_turn_tx(&mut tx, &updated.turn_id)
            .await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &workspace_id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Apply a legal Step transition inside an existing transaction.
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] (code `RUNTIME_ILLEGAL_TRANSITION`) for an
    /// illegal transition and [`GraphError::StateConflict`] when the row changed.
    pub(crate) async fn transition_step_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        to: StepStatus,
    ) -> Result<Step, GraphError> {
        let current = self.lock_step_tx(tx, id).await?;
        if !current.status.can_transition_to(to) {
            return Err(GraphError::IllegalTransition {
                entity: Entity::Step,
                from: current.status.as_db_str().to_string(),
                to: to.as_db_str().to_string(),
            });
        }
        let updated = sqlx::query(
            "UPDATE steps SET status = $1 WHERE id = $2 AND tenant_id = $3 AND status = $4",
        )
        .bind(to.as_db_str())
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(current.status.as_db_str())
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(GraphError::StateConflict {
                entity: Entity::Step.as_str(),
                id: id.to_string(),
                expected: current.status.as_db_str().to_string(),
            });
        }
        self.get_step_tx(tx, id).await
    }

    /// Append a new `started` attempt at the next step sequence.
    ///
    /// Retrying a step calls this again; an earlier attempt row is never reused.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the step is not visible in this tenant.
    pub async fn start_attempt(
        &self,
        step_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Attempt, GraphError> {
        let mut tx = self.begin().await?;
        let created = self.start_attempt_tx(&mut tx, step_id, generation).await?;
        let workspace_id = self
            .workspace_for_step_tx(&mut tx, &created.step_id)
            .await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &workspace_id).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Finish an attempt that is still `started`.
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] when the attempt is already finished, and
    /// [`GraphError::NotFound`] when it is not visible in this tenant.
    pub async fn finish_attempt(
        &self,
        id: &CanonicalId,
        status: AttemptStatus,
        error: Option<Value>,
    ) -> Result<Attempt, GraphError> {
        let mut tx = self.begin().await?;
        let updated = self.finish_attempt_tx(&mut tx, id, status, error).await?;
        let workspace_id = self
            .workspace_for_step_tx(&mut tx, &updated.step_id)
            .await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &workspace_id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// List a step's attempts in sequence order.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_attempts(&self, step_id: &CanonicalId) -> Result<Vec<Attempt>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM attempts WHERE tenant_id = $1 AND step_id = $2 ORDER BY seq"
        ))
        .bind(&self.tenant_id)
        .bind(step_id.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let attempts = rows
            .iter()
            .map(attempt_from_row)
            .collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(attempts)
    }

    // ---------------------------------------------------------------- tx helpers

    pub(crate) async fn create_run_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        run: NewRun,
    ) -> Result<Run, GraphError> {
        Self::ensure_workspace_tx(tx, &self.tenant_id, &run.workspace_id).await?;
        Self::expect_prefix(&run.work_node_id, Prefix::WorkNode)?;
        Self::expect_prefix(&run.agent_thread_id, Prefix::AgentThread)?;
        let node: Option<String> = sqlx::query_scalar(
            "SELECT id FROM work_nodes WHERE id = $1 AND tenant_id = $2 AND workspace_id = $3",
        )
        .bind(run.work_node_id.to_string())
        .bind(&self.tenant_id)
        .bind(&run.workspace_id)
        .fetch_optional(&mut **tx)
        .await?;
        if node.is_none() {
            return Err(GraphError::NotFound {
                entity: Entity::WorkNode.as_str(),
                id: run.work_node_id.to_string(),
                tenant_id: self.tenant_id.clone(),
            });
        }
        let thread: Option<String> = sqlx::query_scalar(
            "SELECT id FROM agent_threads WHERE id = $1 AND tenant_id = $2 AND workspace_id = $3",
        )
        .bind(run.agent_thread_id.to_string())
        .bind(&self.tenant_id)
        .bind(&run.workspace_id)
        .fetch_optional(&mut **tx)
        .await?;
        if thread.is_none() {
            return Err(GraphError::NotFound {
                entity: Entity::AgentThread.as_str(),
                id: run.agent_thread_id.to_string(),
                tenant_id: self.tenant_id.clone(),
            });
        }
        let id = self.generate_id(Prefix::Run);
        sqlx::query(
            "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, \
             generation, status, trigger_kind, trigger_ref, budget_snapshot, execution_target_id) \
             VALUES ($1, $2, $3, $4, $5, $6, 'CREATED', $7, $8, $9, $10)",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(&run.workspace_id)
        .bind(run.work_node_id.to_string())
        .bind(run.agent_thread_id.to_string())
        .bind(Generation::INITIAL.get() as i64)
        .bind(run.trigger_kind.as_db_str())
        .bind(&run.trigger_ref)
        .bind(&run.budget_snapshot)
        .bind(run.execution_target_id.as_ref().map(ToString::to_string))
        .execute(&mut **tx)
        .await?;
        self.get_run_tx(tx, &id).await
    }

    pub(crate) async fn get_run_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Run, GraphError> {
        Self::expect_prefix(id, Prefix::Run)?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE id = $1 AND tenant_id = $2"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => run_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Run.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn lock_run_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Run, GraphError> {
        Self::expect_prefix(id, Prefix::Run)?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => run_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Run.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn transition_run_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        to: RunStatus,
        terminal_reason: Option<String>,
    ) -> Result<Run, GraphError> {
        let current = self.lock_run_tx(tx, id).await?;
        if !current.status.can_transition_to(to) {
            return Err(GraphError::IllegalTransition {
                entity: Entity::Run,
                from: current.status.as_db_str().to_string(),
                to: to.as_db_str().to_string(),
            });
        }
        let updated = sqlx::query(
            "UPDATE runs SET status = $1, \
             terminal_reason = COALESCE($2, terminal_reason), \
             started_at = CASE WHEN $1 = 'RUNNING' AND started_at IS NULL THEN now() ELSE started_at END, \
             ended_at = CASE WHEN $1 IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'BLOCKED_UNRECOVERABLE') \
                             THEN now() ELSE ended_at END \
             WHERE id = $3 AND tenant_id = $4 AND status = $5",
        )
        .bind(to.as_db_str())
        .bind(terminal_reason.as_deref())
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(current.status.as_db_str())
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(GraphError::StateConflict {
                entity: Entity::Run.as_str(),
                id: id.to_string(),
                expected: current.status.as_db_str().to_string(),
            });
        }
        self.get_run_tx(tx, id).await
    }

    pub(crate) async fn create_turn_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        run_id: &CanonicalId,
        turn: NewTurn,
    ) -> Result<Turn, GraphError> {
        self.lock_run_tx(tx, run_id).await?;
        let seq: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(run_id.to_string())
        .fetch_one(&mut **tx)
        .await?;
        let id = self.generate_id(Prefix::Turn);
        sqlx::query(
            "INSERT INTO turns (id, tenant_id, run_id, seq, input_kind, input_ref, \
             context_projection_id, status) VALUES ($1, $2, $3, $4, $5, $6, $7, 'active')",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(run_id.to_string())
        .bind(seq)
        .bind(&turn.input_kind)
        .bind(&turn.input_ref)
        .bind(turn.context_projection_id.as_deref())
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE runs SET current_turn_id = $1 WHERE id = $2 AND tenant_id = $3")
            .bind(id.to_string())
            .bind(run_id.to_string())
            .bind(&self.tenant_id)
            .execute(&mut **tx)
            .await?;
        self.get_turn_tx(tx, &id).await
    }

    pub(crate) async fn get_turn_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Turn, GraphError> {
        let row = self.lock_turn_row_tx(tx, id, false).await?;
        match row {
            Some(row) => turn_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Turn.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn lock_turn_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Turn, GraphError> {
        let row = self.lock_turn_row_tx(tx, id, true).await?;
        match row {
            Some(row) => turn_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Turn.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    async fn lock_turn_row_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        for_update: bool,
    ) -> Result<Option<PgRow>, GraphError> {
        Self::expect_prefix(id, Prefix::Turn)?;
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {TURN_COLUMNS} FROM turns WHERE id = $1 AND tenant_id = $2{lock}"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub(crate) async fn create_step_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        turn_id: &CanonicalId,
        step: NewStep,
    ) -> Result<Step, GraphError> {
        self.lock_turn_tx(tx, turn_id).await?;
        let seq: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE tenant_id = $1 AND turn_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(turn_id.to_string())
        .fetch_one(&mut **tx)
        .await?;
        let id = self.generate_id(Prefix::Step);
        sqlx::query(
            "INSERT INTO steps (id, tenant_id, turn_id, seq, kind, status, ref, effect_id) \
             VALUES ($1, $2, $3, $4, $5, 'pending', $6, $7)",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(turn_id.to_string())
        .bind(seq)
        .bind(step.kind.as_db_str())
        .bind(&step.step_ref)
        .bind(&step.effect_id)
        .execute(&mut **tx)
        .await?;
        self.get_step_tx(tx, &id).await
    }

    pub(crate) async fn get_step_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Step, GraphError> {
        let row = self.lock_step_row_tx(tx, id, false).await?;
        match row {
            Some(row) => step_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Step.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn lock_step_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Step, GraphError> {
        let row = self.lock_step_row_tx(tx, id, true).await?;
        match row {
            Some(row) => step_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Step.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    async fn lock_step_row_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        for_update: bool,
    ) -> Result<Option<PgRow>, GraphError> {
        Self::expect_prefix(id, Prefix::Step)?;
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {STEP_COLUMNS} FROM steps WHERE id = $1 AND tenant_id = $2{lock}"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub(crate) async fn start_attempt_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        step_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Attempt, GraphError> {
        self.lock_step_tx(tx, step_id).await?;
        let seq: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM attempts WHERE tenant_id = $1 AND step_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(step_id.to_string())
        .fetch_one(&mut **tx)
        .await?;
        let id = self.generate_id(Prefix::Attempt);
        sqlx::query(
            "INSERT INTO attempts (id, tenant_id, step_id, seq, generation, status) \
             VALUES ($1, $2, $3, $4, $5, 'started')",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(step_id.to_string())
        .bind(seq)
        .bind(generation.get() as i64)
        .execute(&mut **tx)
        .await?;
        self.get_attempt_tx(tx, &id).await
    }

    pub(crate) async fn get_attempt_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Attempt, GraphError> {
        let row = self.lock_attempt_row_tx(tx, id, false).await?;
        match row {
            Some(row) => attempt_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Attempt.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    async fn lock_attempt_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<Attempt, GraphError> {
        let row = self.lock_attempt_row_tx(tx, id, true).await?;
        match row {
            Some(row) => attempt_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Attempt.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn finish_attempt_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        status: AttemptStatus,
        error: Option<Value>,
    ) -> Result<Attempt, GraphError> {
        let current = self.lock_attempt_tx(tx, id).await?;
        if !current.status.can_transition_to(status) {
            return Err(GraphError::IllegalTransition {
                entity: Entity::Attempt,
                from: current.status.as_db_str().to_string(),
                to: status.as_db_str().to_string(),
            });
        }
        let updated = sqlx::query(
            "UPDATE attempts SET status = $1, error = $2, finished_at = now() \
             WHERE id = $3 AND tenant_id = $4 AND status = $5",
        )
        .bind(status.as_db_str())
        .bind(&error)
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(current.status.as_db_str())
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(GraphError::StateConflict {
                entity: Entity::Attempt.as_str(),
                id: id.to_string(),
                expected: current.status.as_db_str().to_string(),
            });
        }
        self.get_attempt_tx(tx, id).await
    }

    async fn lock_attempt_row_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        for_update: bool,
    ) -> Result<Option<PgRow>, GraphError> {
        Self::expect_prefix(id, Prefix::Attempt)?;
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM attempts WHERE id = $1 AND tenant_id = $2{lock}"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub(crate) async fn workspace_for_run_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        run_id: &CanonicalId,
    ) -> Result<String, GraphError> {
        let workspace: Option<String> =
            sqlx::query_scalar("SELECT workspace_id FROM runs WHERE id = $1 AND tenant_id = $2")
                .bind(run_id.to_string())
                .bind(&self.tenant_id)
                .fetch_optional(&mut **tx)
                .await?;
        workspace.ok_or_else(|| GraphError::NotFound {
            entity: Entity::Run.as_str(),
            id: run_id.to_string(),
            tenant_id: self.tenant_id.clone(),
        })
    }

    async fn workspace_for_turn_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        turn_id: &CanonicalId,
    ) -> Result<String, GraphError> {
        let workspace: Option<String> = sqlx::query_scalar(
            "SELECT r.workspace_id FROM turns t JOIN runs r ON r.id = t.run_id \
             WHERE t.id = $1 AND t.tenant_id = $2 AND r.tenant_id = $2",
        )
        .bind(turn_id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        workspace.ok_or_else(|| GraphError::NotFound {
            entity: Entity::Turn.as_str(),
            id: turn_id.to_string(),
            tenant_id: self.tenant_id.clone(),
        })
    }

    async fn workspace_for_step_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        step_id: &CanonicalId,
    ) -> Result<String, GraphError> {
        let workspace: Option<String> = sqlx::query_scalar(
            "SELECT r.workspace_id FROM steps s \
             JOIN turns t ON t.id = s.turn_id JOIN runs r ON r.id = t.run_id \
             WHERE s.id = $1 AND s.tenant_id = $2",
        )
        .bind(step_id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        workspace.ok_or_else(|| GraphError::NotFound {
            entity: Entity::Step.as_str(),
            id: step_id.to_string(),
            tenant_id: self.tenant_id.clone(),
        })
    }
}
