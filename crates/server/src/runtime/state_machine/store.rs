//! Durable Run/Turn/Step/Attempt state, written with its RuntimeEvent in one transaction.
//!
//! Every method here is one PostgreSQL transaction: the row change and the `run.*`,
//! `turn.*` or `step.*` RuntimeEvent it records are committed together through
//! [`EventStore::commit_mutation_tx`] — the same transactional event primitive
//! `crates/graph`'s `GraphTransaction` builds on, and the store refuses to commit a
//! transaction that staged no event. A rejected transition (illegal state, stale
//! generation, mismatched wait) rolls back, so state and event log never disagree.
//!
//! Attempts are appended, never updated into a new identity: dispatching a step inserts
//! the next `seq` for that step, so a retry is a new `att_…` row and the earlier attempt
//! is left exactly as it finished (DOMAIN.md §5.5).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use quansio_core::{
    CanonicalId, CausationId, CommandId, CorrelationId, Generation, Prefix, UlidGenerator,
};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::control::schema;
use crate::runtime::protocol_state::{
    ProtocolState, ProtocolStateError, ProtocolStateStore, WaitKind,
};

use super::{
    fence, AttemptStatus, RunStatus, RunTriggerKind, RuntimeError, RuntimeIdentity, StepKind,
    StepStatus, TurnStatus,
};

/// Repository path used to attribute a rejected mutation.
pub const RUNTIME_OWNER: &str = "crates/server/src/runtime";

/// Boxed future returned by a runtime mutation closure.
pub type BoxRuntimeFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RuntimeError>> + Send + 'a>>;

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

/// Default step budget for a Run whose snapshot does not set `max_steps`.
pub const DEFAULT_MAX_STEPS: u32 = 16;

/// The step budget the turn loop honours for one Turn (DOMAIN.md §13.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Maximum steps one Turn may record.
    pub max_steps: u32,
}

impl Budget {
    /// A budget of `max_steps` steps.
    #[must_use]
    pub const fn new(max_steps: u32) -> Self {
        Self { max_steps }
    }

    /// Read the budget from a Run's `budget_snapshot`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::InvalidArgument`] when `max_steps` is present but not a
    /// non-negative integer that fits in `u32`.
    pub fn from_snapshot(snapshot: &Value) -> Result<Self, RuntimeError> {
        let Some(raw) = snapshot.get("max_steps") else {
            return Ok(Self::new(DEFAULT_MAX_STEPS));
        };
        if raw.is_null() {
            return Ok(Self::new(DEFAULT_MAX_STEPS));
        }
        let value = raw.as_u64().ok_or_else(|| {
            RuntimeError::InvalidArgument(format!(
                "budget_snapshot.max_steps is not an integer: {raw}"
            ))
        })?;
        let max_steps = u32::try_from(value).map_err(|_| {
            RuntimeError::InvalidArgument(format!(
                "budget_snapshot.max_steps is out of range: {value}"
            ))
        })?;
        Ok(Self::new(max_steps))
    }
}

/// A Run to create (DOMAIN.md §5.2).
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
    pub execution_target_id: Option<String>,
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

    /// Set the budget snapshot.
    #[must_use]
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget_snapshot = json!({ "max_steps": budget.max_steps });
        self
    }
}

/// A persisted `runs` row (DOMAIN.md §5.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Canonical `run_…` identity.
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
    pub execution_target_id: Option<String>,
    /// When the run started executing.
    pub started_at: Option<DateTime<Utc>>,
    /// When the run reached a terminal state.
    pub ended_at: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// The input that starts a Turn (DOMAIN.md §5.3, §5.6).
#[derive(Debug, Clone)]
pub struct TurnInput {
    /// Input kind.
    pub kind: RunTriggerKind,
    /// Input reference (message id, routine id, child run id, …).
    pub reference: Option<String>,
    /// ContextProjection built for this turn (`ctx_…`; carried opaquely).
    pub context_projection_id: Option<String>,
}

impl TurnInput {
    /// An input of the given kind.
    #[must_use]
    pub fn new(kind: RunTriggerKind) -> Self {
        Self {
            kind,
            reference: None,
            context_projection_id: None,
        }
    }

    /// Set the input reference.
    #[must_use]
    pub fn with_reference(mut self, reference: impl Into<String>) -> Self {
        self.reference = Some(reference.into());
        self
    }
}

/// A persisted `turns` row (DOMAIN.md §5.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// Canonical `trn_…` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Parent run.
    pub run_id: CanonicalId,
    /// Run-monotonic sequence.
    pub seq: i32,
    /// Input kind.
    pub input_kind: RunTriggerKind,
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

/// A Step to append (DOMAIN.md §5.4).
#[derive(Debug, Clone)]
pub struct NewStep {
    /// Step kind.
    pub kind: StepKind,
    /// Dispatch reference.
    pub step_ref: Option<String>,
    /// EffectRecord reserved by this step.
    pub effect_id: Option<String>,
}

impl NewStep {
    /// A step of the given kind.
    #[must_use]
    pub fn new(kind: StepKind) -> Self {
        Self {
            kind,
            step_ref: None,
            effect_id: None,
        }
    }

    /// Set the dispatch reference.
    #[must_use]
    pub fn with_ref(mut self, reference: impl Into<String>) -> Self {
        self.step_ref = Some(reference.into());
        self
    }
}

/// A persisted `steps` row (DOMAIN.md §5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// Canonical `stp_…` identity.
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
    /// Canonical `att_…` identity.
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

/// A resolution that releases a parked Run (DOMAIN.md §5.2, §5.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitResolution {
    /// An approval receipt for a pending approval request.
    Approval {
        /// Approval request id (`apr_…`).
        approval_id: String,
    },
    /// A human answer to an open question.
    Question {
        /// Question id (`q_…`).
        question_id: String,
    },
    /// An external event with the awaited key.
    Event {
        /// Wait key.
        key: String,
    },
    /// A durable timer that fired.
    Timer {
        /// Wait key.
        key: String,
    },
    /// A child agent thread that joined.
    Child {
        /// Child AgentThread id (`ath_…`).
        agent_thread_id: String,
    },
    /// A human who held browser/computer control handed back.
    Takeover {
        /// Browser session id.
        session_id: String,
    },
}

impl WaitResolution {
    /// The Run state this resolution releases.
    #[must_use]
    pub const fn waiting_state(&self) -> RunStatus {
        match self {
            Self::Approval { .. } => RunStatus::WaitingApproval,
            Self::Question { .. } => RunStatus::WaitingQuestion,
            Self::Event { .. } => RunStatus::WaitingEvent,
            Self::Timer { .. } => RunStatus::WaitingTimer,
            Self::Child { .. } => RunStatus::WaitingChild,
            Self::Takeover { .. } => RunStatus::WaitingTakeover,
        }
    }

    /// The key this resolution names, for typed diagnostics.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Approval { approval_id } => approval_id,
            Self::Question { question_id } => question_id,
            Self::Event { key } | Self::Timer { key } => key,
            Self::Child { agent_thread_id } => agent_thread_id,
            Self::Takeover { session_id } => session_id,
        }
    }

    /// Remove the matching entry from a copy of the protocol state, or report the wait
    /// the inbound resolution failed to match.
    fn release(&self, state: &ProtocolState) -> Result<ProtocolState, String> {
        let mut updated = state.clone();
        let expected = state
            .pending_approvals
            .first()
            .or_else(|| state.open_questions.first())
            .or_else(|| state.waits.first().map(|wait| &wait.key))
            .or_else(|| state.child_agent_threads.first())
            .or_else(|| {
                state
                    .browser_control
                    .as_ref()
                    .map(|control| &control.session_id)
            });
        match self {
            Self::Approval { approval_id } => {
                if let Some(index) = updated
                    .pending_approvals
                    .iter()
                    .position(|id| id == approval_id)
                {
                    updated.pending_approvals.remove(index);
                    return Ok(updated);
                }
            }
            Self::Question { question_id } => {
                if let Some(index) = updated
                    .open_questions
                    .iter()
                    .position(|id| id == question_id)
                {
                    updated.open_questions.remove(index);
                    return Ok(updated);
                }
            }
            Self::Event { key } => {
                if let Some(index) = updated
                    .waits
                    .iter()
                    .position(|wait| wait.kind == WaitKind::Event && &wait.key == key)
                {
                    updated.waits.remove(index);
                    return Ok(updated);
                }
            }
            Self::Timer { key } => {
                if let Some(index) = updated
                    .waits
                    .iter()
                    .position(|wait| wait.kind == WaitKind::Timer && &wait.key == key)
                {
                    updated.waits.remove(index);
                    return Ok(updated);
                }
            }
            Self::Child { agent_thread_id } => {
                if let Some(index) = updated
                    .child_agent_threads
                    .iter()
                    .position(|id| id == agent_thread_id)
                {
                    updated.child_agent_threads.remove(index);
                    return Ok(updated);
                }
            }
            Self::Takeover { session_id } => {
                let matches = updated
                    .browser_control
                    .as_ref()
                    .is_some_and(|control| &control.session_id == session_id);
                if matches {
                    updated.browser_control = None;
                    return Ok(updated);
                }
            }
        }
        Err(expected.map_or_else(|| "no pending wait".to_string(), ToString::to_string))
    }
}

/// The Run/Turn/Step/Attempt store (DOMAIN.md §5.2–§5.5).
#[derive(Debug, Clone)]
pub struct RuntimeStore {
    events: EventStore,
    identity: RuntimeIdentity,
}

impl RuntimeStore {
    /// Bind a store to one tenant and event identity.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when `identity.tenant_id` is not a canonical `tn_`
    /// identifier.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        schema::validate_tenant_id(&identity.tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool),
            identity,
        })
    }

    /// The event identity every mutation is stamped with.
    #[must_use]
    pub fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    /// The PostgreSQL pool this store writes through.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        self.events.pool()
    }

    /// Run one mutation and its events in a single transaction, or commit nothing.
    async fn commit<T, F>(&self, mutation: F) -> Result<T, RuntimeError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxRuntimeFuture<'a, T>,
    {
        let tenant_id = self.identity.tenant_id.clone();
        let rejection: Arc<Mutex<Option<RuntimeError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(&tenant_id, move |tx, batch| {
                Box::pin(async move {
                    match mutation(tx, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: RUNTIME_OWNER,
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(EventError::MutationRejected { .. }) => {
                let error = rejection
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                Err(error.unwrap_or_else(|| {
                    RuntimeError::Event(EventError::MutationRejected {
                        owner: RUNTIME_OWNER,
                        message: "transaction rejected without a recorded reason".to_string(),
                    })
                }))
            }
            Err(error) => Err(RuntimeError::Event(error)),
        }
    }

    // ------------------------------------------------------------------ run lifecycle

    /// Create a Run in `CREATED` and emit `run.created`.
    ///
    /// # Errors
    /// Returns a database error when the workspace, WorkNode or AgentThread is not
    /// visible in this tenant.
    pub async fn create_run(&self, run: NewRun) -> Result<Run, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let id = new_id(Prefix::Run);
                sqlx::query(
                    "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, \
                     generation, status, trigger_kind, trigger_ref, budget_snapshot, execution_target_id) \
                     VALUES ($1, $2, $3, $4, $5, $6, 'CREATED', $7, $8, $9, $10)",
                )
                .bind(id.to_string())
                .bind(&tenant_id)
                .bind(&run.workspace_id)
                .bind(run.work_node_id.to_string())
                .bind(run.agent_thread_id.to_string())
                .bind(Generation::INITIAL.get() as i64)
                .bind(run.trigger_kind.as_db_str())
                .bind(run.trigger_ref.as_deref())
                .bind(&run.budget_snapshot)
                .bind(run.execution_target_id.as_deref())
                .execute(&mut **tx)
                .await?;
                let created = load_run_tx(tx, &tenant_id, &id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "run",
                        &created.id.to_string(),
                        "run.created",
                        &created.workspace_id,
                        created.generation,
                        json!({
                            "run_id": created.id.to_string(),
                            "workspace_id": created.workspace_id,
                            "work_node_id": created.work_node_id.to_string(),
                            "agent_thread_id": created.agent_thread_id.to_string(),
                            "status": created.status.as_db_str(),
                            "trigger_kind": created.trigger_kind.as_db_str(),
                            "trigger_ref": created.trigger_ref,
                            "generation": created.generation.get(),
                        }),
                    )
                    .await?;
                Ok(created)
            })
        })
        .await
    }

    /// Move a Run `CREATED → QUEUED` (DOMAIN.md §5.2).
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] for any other source state.
    pub async fn enqueue(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        self.transition_run(run_id, generation, RunStatus::Queued, None)
            .await
    }

    /// Move a Run `QUEUED → RUNNING` (DOMAIN.md §5.2).
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] for any other source state.
    pub async fn start(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        self.transition_run(run_id, generation, RunStatus::Running, None)
            .await
    }

    /// Apply one legal Run transition and emit its `run.*` event.
    ///
    /// # Errors
    /// Returns [`RuntimeError::FencedStaleGeneration`] when `generation` is behind the
    /// Run's generation, [`RuntimeError::IllegalTransition`] when DOMAIN.md §5.2 does not
    /// allow it, and [`RuntimeError::NotFound`] when the Run is not visible in this
    /// tenant. Nothing is written on error.
    pub async fn transition_run(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        to: RunStatus,
        terminal_reason: Option<String>,
    ) -> Result<Run, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let run_id = *run_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                apply_run_transition(
                    tx,
                    batch,
                    &mut publisher,
                    &tenant_id,
                    &run_id,
                    generation,
                    to,
                    terminal_reason,
                )
                .await
            })
        })
        .await
    }

    /// Request cancellation authoritatively (DOMAIN.md §5.2).
    ///
    /// A Run already in `CANCELLED` is returned unchanged and emits nothing, so calling
    /// this twice produces exactly one `run.cancelled` event. The durable protocol state
    /// records the request in the same transaction, so recovery honours a cancellation
    /// that was requested before a crash.
    ///
    /// # Errors
    /// Returns [`RuntimeError::FencedStaleGeneration`] for a stale generation and
    /// [`RuntimeError::IllegalTransition`] when the Run cannot be cancelled.
    pub async fn cancel(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        // An already-cancelled run is returned as-is: a transaction that stages no event
        // is refused by the event store, and a second cancellation must not add one.
        let current = self.load_run(run_id).await?;
        fence(current.generation, generation)?;
        if current.status == RunStatus::Cancelled {
            return Ok(current);
        }
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let run_id = *run_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let current = lock_run(tx, &tenant_id, &run_id).await?;
                fence(current.generation, generation)?;
                if current.status == RunStatus::Cancelled {
                    return Err(RuntimeError::StateConflict {
                        entity: "run",
                        id: run_id.to_string(),
                    });
                }
                if let Some(mut state) =
                    ProtocolStateStore::load(tx, &tenant_id, &run_id.to_string())
                        .await
                        .map_err(RuntimeError::from)?
                {
                    state.cancellation_requested = true;
                    state.cancellation_at = current.updated_at.to_rfc3339().into();
                    state.generation = current.generation.get() as i64;
                    ProtocolStateStore::store(tx, &tenant_id, &state)
                        .await
                        .map_err(RuntimeError::from)?;
                }
                apply_run_transition(
                    tx,
                    batch,
                    &mut publisher,
                    &tenant_id,
                    &run_id,
                    generation,
                    RunStatus::Cancelled,
                    Some("cancellation requested".to_string()),
                )
                .await
            })
        })
        .await
    }

    /// Suspend a Run with a typed reason (DOMAIN.md §5.2).
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] unless the Run is `RUNNING` or in a
    /// `WAITING_*` state.
    pub async fn suspend(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        reason: impl Into<String>,
    ) -> Result<Run, RuntimeError> {
        self.transition_run(
            run_id,
            generation,
            RunStatus::Suspended,
            Some(reason.into()),
        )
        .await
    }

    /// Resume a suspended Run (DOMAIN.md §5.2).
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] when the Run is not `SUSPENDED` and
    /// [`RuntimeError::FencedStaleGeneration`] when `generation` is stale.
    pub async fn resume(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let run_id = *run_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let current = lock_run(tx, &tenant_id, &run_id).await?;
                fence(current.generation, generation)?;
                if current.status != RunStatus::Suspended {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "run",
                        from: current.status.as_db_str().to_string(),
                        to: RunStatus::Running.as_db_str().to_string(),
                    });
                }
                apply_run_transition(
                    tx,
                    batch,
                    &mut publisher,
                    &tenant_id,
                    &run_id,
                    generation,
                    RunStatus::Running,
                    None,
                )
                .await
            })
        })
        .await
    }

    /// Release a parked Run with its matching resolution only (DOMAIN.md §5.2, §5.7).
    ///
    /// # Errors
    /// Returns [`RuntimeError::WaitMismatch`] when the Run is not in the resolution's
    /// waiting state or the named wait is not pending. Nothing is written on mismatch.
    pub async fn resolve_wait(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        resolution: WaitResolution,
    ) -> Result<Run, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let run_id = *run_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let current = lock_run(tx, &tenant_id, &run_id).await?;
                fence(current.generation, generation)?;
                if current.status != resolution.waiting_state() {
                    return Err(RuntimeError::WaitMismatch {
                        run_id: run_id.to_string(),
                        expected: resolution.waiting_state().as_db_str().to_string(),
                        received: current.status.as_db_str().to_string(),
                    });
                }
                let state = ProtocolStateStore::load(tx, &tenant_id, &run_id.to_string())
                    .await
                    .map_err(RuntimeError::from)?
                    .ok_or_else(|| RuntimeError::WaitMismatch {
                        run_id: run_id.to_string(),
                        expected: resolution.key().to_string(),
                        received: "no durable protocol state".to_string(),
                    })?;
                let updated = match resolution.release(&state) {
                    Ok(updated) => updated,
                    Err(pending) => {
                        return Err(RuntimeError::WaitMismatch {
                            run_id: run_id.to_string(),
                            expected: pending,
                            received: resolution.key().to_string(),
                        })
                    }
                };
                ProtocolStateStore::store(tx, &tenant_id, &updated)
                    .await
                    .map_err(RuntimeError::from)?;
                apply_run_transition(
                    tx,
                    batch,
                    &mut publisher,
                    &tenant_id,
                    &run_id,
                    generation,
                    RunStatus::Running,
                    None,
                )
                .await
            })
        })
        .await
    }

    // ------------------------------------------------------------- turn and step rows

    /// Start a Turn on a `RUNNING` Run and emit `turn.started`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] when the Run is not `RUNNING` and
    /// [`RuntimeError::FencedStaleGeneration`] for a stale generation.
    pub async fn begin_turn(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        input: TurnInput,
    ) -> Result<Turn, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let run_id = *run_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let run = lock_run(tx, &tenant_id, &run_id).await?;
                fence(run.generation, generation)?;
                if run.status != RunStatus::Running {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "run",
                        from: run.status.as_db_str().to_string(),
                        to: RunStatus::Running.as_db_str().to_string(),
                    });
                }
                let turn = insert_turn(tx, &tenant_id, &run, &input).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "turn",
                        &turn.id.to_string(),
                        "turn.started",
                        &run.workspace_id,
                        generation,
                        json!({
                            "turn_id": turn.id.to_string(),
                            "run_id": turn.run_id.to_string(),
                            "seq": turn.seq,
                            "status": turn.status.as_db_str(),
                            "input_kind": turn.input_kind.as_db_str(),
                            "input_ref": turn.input_ref,
                        }),
                    )
                    .await?;
                Ok(turn)
            })
        })
        .await
    }

    /// Finish a Turn (`active → completed | aborted`) and emit `turn.completed`/`turn.aborted`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] when the Turn is already finished.
    pub async fn finish_turn(
        &self,
        turn_id: &CanonicalId,
        generation: Generation,
        status: TurnStatus,
    ) -> Result<Turn, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let turn_id = *turn_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let turn = lock_turn(tx, &tenant_id, &turn_id).await?;
                let run = lock_run(tx, &tenant_id, &turn.run_id).await?;
                fence(run.generation, generation)?;
                if !turn.status.can_transition_to(status) {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "turn",
                        from: turn.status.as_db_str().to_string(),
                        to: status.as_db_str().to_string(),
                    });
                }
                let updated = sqlx::query(
                    "UPDATE turns SET status = $1, ended_at = now() \
                     WHERE id = $2 AND tenant_id = $3 AND status = $4",
                )
                .bind(status.as_db_str())
                .bind(turn.id.to_string())
                .bind(&tenant_id)
                .bind(turn.status.as_db_str())
                .execute(&mut **tx)
                .await?;
                if updated.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "turn",
                        id: turn.id.to_string(),
                    });
                }
                let updated = load_turn_tx(tx, &tenant_id, &turn.id).await?;
                let event_type = match status {
                    TurnStatus::Completed => "turn.completed",
                    TurnStatus::Aborted => "turn.aborted",
                    TurnStatus::Active => {
                        return Err(RuntimeError::IllegalTransition {
                            entity: "turn",
                            from: turn.status.as_db_str().to_string(),
                            to: status.as_db_str().to_string(),
                        })
                    }
                };
                publisher
                    .emit(
                        tx,
                        batch,
                        "turn",
                        &updated.id.to_string(),
                        event_type,
                        &run.workspace_id,
                        generation,
                        json!({
                            "turn_id": updated.id.to_string(),
                            "run_id": updated.run_id.to_string(),
                            "seq": updated.seq,
                            "status": updated.status.as_db_str(),
                            "step_count": updated.step_count,
                        }),
                    )
                    .await?;
                Ok(updated)
            })
        })
        .await
    }

    /// Append a Step to an `active` Turn and emit `step.created`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the Turn is not visible in this tenant and
    /// [`RuntimeError::IllegalTransition`] when the Turn is not active.
    pub async fn record_step(
        &self,
        turn_id: &CanonicalId,
        generation: Generation,
        step: NewStep,
    ) -> Result<Step, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let turn_id = *turn_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let turn = lock_turn(tx, &tenant_id, &turn_id).await?;
                let run = lock_run(tx, &tenant_id, &turn.run_id).await?;
                fence(run.generation, generation)?;
                if turn.status != TurnStatus::Active {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "turn",
                        from: turn.status.as_db_str().to_string(),
                        to: TurnStatus::Active.as_db_str().to_string(),
                    });
                }
                let seq: i32 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE tenant_id = $1 AND turn_id = $2",
                )
                .bind(&tenant_id)
                .bind(turn.id.to_string())
                .fetch_one(&mut **tx)
                .await?;
                let id = new_id(Prefix::Step);
                sqlx::query(
                    "INSERT INTO steps (id, tenant_id, turn_id, seq, kind, status, ref, effect_id) \
                     VALUES ($1, $2, $3, $4, $5, 'pending', $6, $7)",
                )
                .bind(id.to_string())
                .bind(&tenant_id)
                .bind(turn.id.to_string())
                .bind(seq)
                .bind(step.kind.as_db_str())
                .bind(step.step_ref.as_deref())
                .bind(step.effect_id.as_deref())
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "UPDATE turns SET step_count = step_count + 1 WHERE id = $1 AND tenant_id = $2",
                )
                .bind(turn.id.to_string())
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                let created = load_step_tx(tx, &tenant_id, &id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "step",
                        &created.id.to_string(),
                        "step.created",
                        &run.workspace_id,
                        generation,
                        json!({
                            "step_id": created.id.to_string(),
                            "turn_id": created.turn_id.to_string(),
                            "run_id": run.id.to_string(),
                            "seq": created.seq,
                            "kind": created.kind.as_db_str(),
                            "status": created.status.as_db_str(),
                            "ref": created.step_ref,
                        }),
                    )
                    .await?;
                Ok(created)
            })
        })
        .await
    }

    /// Dispatch a Step and append a new Attempt, emitting `step.dispatched`.
    ///
    /// Dispatching a step that is already `dispatched` is the retry primitive of
    /// DOMAIN.md §5.5: it appends the next Attempt and leaves the earlier one untouched.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] for a terminal or `unknown` Step.
    pub async fn dispatch_step(
        &self,
        step_id: &CanonicalId,
        generation: Generation,
    ) -> Result<(Step, Attempt), RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let step_id = *step_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let step = lock_step(tx, &tenant_id, &step_id).await?;
                let run = lock_run_for_step(tx, &tenant_id, &step).await?;
                fence(run.generation, generation)?;
                if !matches!(step.status, StepStatus::Pending | StepStatus::Dispatched) {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "step",
                        from: step.status.as_db_str().to_string(),
                        to: StepStatus::Dispatched.as_db_str().to_string(),
                    });
                }
                if step.status == StepStatus::Pending {
                    let updated = sqlx::query(
                        "UPDATE steps SET status = 'dispatched' \
                         WHERE id = $1 AND tenant_id = $2 AND status = 'pending'",
                    )
                    .bind(step.id.to_string())
                    .bind(&tenant_id)
                    .execute(&mut **tx)
                    .await?;
                    if updated.rows_affected() != 1 {
                        return Err(RuntimeError::StateConflict {
                            entity: "step",
                            id: step.id.to_string(),
                        });
                    }
                }
                let seq: i32 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM attempts WHERE tenant_id = $1 AND step_id = $2",
                )
                .bind(&tenant_id)
                .bind(step.id.to_string())
                .fetch_one(&mut **tx)
                .await?;
                let attempt_id = new_id(Prefix::Attempt);
                sqlx::query(
                    "INSERT INTO attempts (id, tenant_id, step_id, seq, generation, status) \
                     VALUES ($1, $2, $3, $4, $5, 'started')",
                )
                .bind(attempt_id.to_string())
                .bind(&tenant_id)
                .bind(step.id.to_string())
                .bind(seq)
                .bind(generation.get() as i64)
                .execute(&mut **tx)
                .await?;
                let updated_step = load_step_tx(tx, &tenant_id, &step.id).await?;
                let attempt = load_attempt_tx(tx, &tenant_id, &attempt_id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "step",
                        &updated_step.id.to_string(),
                        "step.dispatched",
                        &run.workspace_id,
                        generation,
                        json!({
                            "step_id": updated_step.id.to_string(),
                            "turn_id": updated_step.turn_id.to_string(),
                            "run_id": run.id.to_string(),
                            "seq": updated_step.seq,
                            "kind": updated_step.kind.as_db_str(),
                            "status": updated_step.status.as_db_str(),
                            "attempt_id": attempt.id.to_string(),
                            "attempt_seq": attempt.seq,
                        }),
                    )
                    .await?;
                Ok((updated_step, attempt))
            })
        })
        .await
    }

    /// Finish the Attempt and move the Step, emitting the matching `step.*` event.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] when the Step or Attempt cannot take
    /// the requested state, and [`RuntimeError::NotFound`] when either is not visible.
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_step(
        &self,
        step_id: &CanonicalId,
        generation: Generation,
        attempt_id: &CanonicalId,
        step_status: StepStatus,
        attempt_status: AttemptStatus,
        error: Option<Value>,
        evidence_ids: Vec<String>,
    ) -> Result<Step, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let step_id = *step_id;
        let attempt_id = *attempt_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let step = lock_step(tx, &tenant_id, &step_id).await?;
                let run = lock_run_for_step(tx, &tenant_id, &step).await?;
                fence(run.generation, generation)?;
                if !step.status.can_transition_to(step_status) {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "step",
                        from: step.status.as_db_str().to_string(),
                        to: step_status.as_db_str().to_string(),
                    });
                }
                let attempt = lock_attempt(tx, &tenant_id, &attempt_id).await?;
                if attempt.step_id != step.id {
                    return Err(RuntimeError::NotFound {
                        entity: "attempt",
                        id: attempt.id.to_string(),
                        tenant_id: tenant_id.clone(),
                    });
                }
                if !attempt.status.can_transition_to(attempt_status) {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "attempt",
                        from: attempt.status.as_db_str().to_string(),
                        to: attempt_status.as_db_str().to_string(),
                    });
                }
                let finished = sqlx::query(
                    "UPDATE attempts SET status = $1, error = $2, finished_at = now() \
                     WHERE id = $3 AND tenant_id = $4 AND status = 'started'",
                )
                .bind(attempt_status.as_db_str())
                .bind(&error)
                .bind(attempt.id.to_string())
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                if finished.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "attempt",
                        id: attempt.id.to_string(),
                    });
                }
                let evidence = serde_json::to_value(&evidence_ids)?;
                let updated = sqlx::query(
                    "UPDATE steps SET status = $1, evidence_ids = $2 \
                     WHERE id = $3 AND tenant_id = $4 AND status = $5",
                )
                .bind(step_status.as_db_str())
                .bind(&evidence)
                .bind(step.id.to_string())
                .bind(&tenant_id)
                .bind(step.status.as_db_str())
                .execute(&mut **tx)
                .await?;
                if updated.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "step",
                        id: step.id.to_string(),
                    });
                }
                let updated = load_step_tx(tx, &tenant_id, &step.id).await?;
                let event_type = step_event_name(step_status).ok_or_else(|| {
                    RuntimeError::IllegalTransition {
                        entity: "step",
                        from: step.status.as_db_str().to_string(),
                        to: step_status.as_db_str().to_string(),
                    }
                })?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "step",
                        &updated.id.to_string(),
                        event_type,
                        &run.workspace_id,
                        generation,
                        json!({
                            "step_id": updated.id.to_string(),
                            "turn_id": updated.turn_id.to_string(),
                            "run_id": run.id.to_string(),
                            "seq": updated.seq,
                            "kind": updated.kind.as_db_str(),
                            "status": updated.status.as_db_str(),
                            "attempt_id": attempt.id.to_string(),
                            "attempt_status": attempt_status.as_db_str(),
                            "evidence_ids": updated.evidence_ids,
                        }),
                    )
                    .await?;
                Ok(updated)
            })
        })
        .await
    }

    // -------------------------------------------------------------------- read paths

    /// Read one Run.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the Run is not visible in this tenant.
    pub async fn load_run(&self, run_id: &CanonicalId) -> Result<Run, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let run_id = *run_id;
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let run = load_run_tx(&mut tx, &tenant_id, &run_id).await?;
        tx.commit().await?;
        Ok(run)
    }

    /// Read one Turn.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the Turn is not visible in this tenant.
    pub async fn load_turn(&self, turn_id: &CanonicalId) -> Result<Turn, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let turn_id = *turn_id;
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let turn = load_turn_tx(&mut tx, &tenant_id, &turn_id).await?;
        tx.commit().await?;
        Ok(turn)
    }

    /// List a Run's Turns in sequence order.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_turns(&self, run_id: &CanonicalId) -> Result<Vec<Turn>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let run_id = *run_id;
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {TURN_COLUMNS} FROM turns WHERE tenant_id = $1 AND run_id = $2 ORDER BY seq"
        ))
        .bind(&tenant_id)
        .bind(run_id.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let turns = rows.iter().map(turn_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(turns)
    }

    /// List a Turn's Steps in sequence order.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_steps(&self, turn_id: &CanonicalId) -> Result<Vec<Step>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let turn_id = *turn_id;
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {STEP_COLUMNS} FROM steps WHERE tenant_id = $1 AND turn_id = $2 ORDER BY seq"
        ))
        .bind(&tenant_id)
        .bind(turn_id.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let steps = rows.iter().map(step_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(steps)
    }

    /// Read one Step.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the Step is not visible in this tenant.
    pub async fn load_step(&self, step_id: &CanonicalId) -> Result<Step, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let step_id = *step_id;
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let step = load_step_tx(&mut tx, &tenant_id, &step_id).await?;
        tx.commit().await?;
        Ok(step)
    }

    /// List a Step's Attempts in sequence order.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_attempts(&self, step_id: &CanonicalId) -> Result<Vec<Attempt>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let step_id = *step_id;
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM attempts WHERE tenant_id = $1 AND step_id = $2 ORDER BY seq"
        ))
        .bind(&tenant_id)
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

    /// Read a Run's durable protocol state (DOMAIN.md §5.7).
    ///
    /// # Errors
    /// Returns a database or decode error when the query fails.
    pub async fn load_protocol_state(
        &self,
        run_id: &CanonicalId,
    ) -> Result<Option<ProtocolState>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let state = ProtocolStateStore::load(&mut tx, &tenant_id, &run_id.to_string())
            .await
            .map_err(RuntimeError::from)?;
        tx.commit().await?;
        Ok(state)
    }

    /// Replace a Run's durable protocol state (used by recovery and the turn loop).
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the Run is not visible in this tenant.
    pub async fn store_protocol_state(&self, state: &ProtocolState) -> Result<(), RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let run_id = CanonicalId::parse_typed(&state.run_id, Prefix::Run)?;
        if load_run_tx(&mut tx, &tenant_id, &run_id).await.is_err() {
            return Err(RuntimeError::NotFound {
                entity: "run",
                id: state.run_id.clone(),
                tenant_id,
            });
        }
        ProtocolStateStore::store(&mut tx, &tenant_id, state)
            .await
            .map_err(RuntimeError::from)?;
        tx.commit().await?;
        Ok(())
    }
}

/// The `run.*` event name for a transition (DOMAIN.md §9.2).
#[must_use]
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

/// The `step.*` event name for a step status (DOMAIN.md §9.2).
#[must_use]
pub(crate) const fn step_event_name(status: StepStatus) -> Option<&'static str> {
    match status {
        StepStatus::Pending => None,
        StepStatus::Dispatched => Some("step.dispatched"),
        StepStatus::Completed => Some("step.completed"),
        StepStatus::Failed => Some("step.failed"),
        StepStatus::Cancelled => Some("step.cancelled"),
        StepStatus::Unknown => Some("step.unknown"),
    }
}

fn new_id(prefix: Prefix) -> CanonicalId {
    let mut generator = UlidGenerator::new();
    CanonicalId::generate(prefix, &mut generator)
}

/// Stamps every RuntimeEvent of one transaction with the same identity and version.
struct Publisher<'a> {
    tenant_id: &'a str,
    actor: &'a Actor,
    correlation_id: CorrelationId,
    causation_id: Option<&'a CausationId>,
    command_id: Option<CommandId>,
    versions: HashMap<(String, String), u64>,
}

impl<'a> Publisher<'a> {
    fn new(tenant_id: &'a str, identity: &'a RuntimeIdentity) -> Self {
        Self {
            tenant_id,
            actor: &identity.actor,
            correlation_id: identity.correlation_id,
            causation_id: identity.causation_id.as_ref(),
            command_id: identity.command_id,
            versions: HashMap::new(),
        }
    }

    /// Stage one RuntimeEvent whose version is the aggregate's next committed version.
    #[allow(clippy::too_many_arguments)]
    async fn emit(
        &mut self,
        tx: &mut Transaction<'static, Postgres>,
        batch: &mut EventBatch,
        aggregate_type: &str,
        aggregate_id: &str,
        event_type: &str,
        workspace_id: &str,
        generation: Generation,
        payload: Value,
    ) -> Result<(), RuntimeError> {
        let key = (aggregate_type.to_string(), aggregate_id.to_string());
        let version = match self.versions.get_mut(&key) {
            Some(version) => {
                *version += 1;
                *version
            }
            None => {
                let existing: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 \
                     AND aggregate_type = $2 AND aggregate_id = $3",
                )
                .bind(self.tenant_id)
                .bind(aggregate_type)
                .bind(aggregate_id)
                .fetch_one(&mut **tx)
                .await?;
                let next = u64::try_from(existing).unwrap_or(0) + 1;
                self.versions.insert(key, next);
                next
            }
        };
        let event_type = EventType::parse(event_type)?;
        let mut draft = EventDraft::new(
            aggregate_type,
            aggregate_id,
            version,
            event_type,
            self.correlation_id,
            self.actor.clone(),
        )
        .with_workspace(workspace_id)
        .with_generation(generation)
        .with_payload(payload);
        if let Some(command_id) = self.command_id {
            draft = draft.with_command_id(command_id);
        }
        if let Some(causation_id) = self.causation_id {
            draft = draft.with_causation_id(causation_id.clone());
        }
        batch.emit(draft);
        Ok(())
    }
}

/// Apply one Run transition on an open transaction: fence, check legality, compare-and-set
/// the row, then stage the matching `run.*` event.
#[allow(clippy::too_many_arguments)]
async fn apply_run_transition(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    publisher: &mut Publisher<'_>,
    tenant_id: &str,
    run_id: &CanonicalId,
    generation: Generation,
    to: RunStatus,
    terminal_reason: Option<String>,
) -> Result<Run, RuntimeError> {
    let current = lock_run(tx, tenant_id, run_id).await?;
    fence(current.generation, generation)?;
    if !current.status.can_transition_to(to) {
        return Err(RuntimeError::IllegalTransition {
            entity: "run",
            from: current.status.as_db_str().to_string(),
            to: to.as_db_str().to_string(),
        });
    }
    let updated = sqlx::query(
        "UPDATE runs SET status = $1, terminal_reason = COALESCE($2, terminal_reason), \
         started_at = CASE WHEN $1 = 'RUNNING' AND started_at IS NULL THEN now() ELSE started_at END, \
         ended_at = CASE WHEN $1 IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'BLOCKED_UNRECOVERABLE') \
                         THEN now() ELSE ended_at END \
         WHERE id = $3 AND tenant_id = $4 AND status = $5",
    )
    .bind(to.as_db_str())
    .bind(terminal_reason.as_deref())
    .bind(run_id.to_string())
    .bind(tenant_id)
    .bind(current.status.as_db_str())
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(RuntimeError::StateConflict {
            entity: "run",
            id: run_id.to_string(),
        });
    }
    let run = load_run_tx(tx, tenant_id, run_id).await?;
    let event_type =
        run_event_name(current.status, to).ok_or_else(|| RuntimeError::IllegalTransition {
            entity: "run",
            from: current.status.as_db_str().to_string(),
            to: to.as_db_str().to_string(),
        })?;
    publisher
        .emit(
            tx,
            batch,
            "run",
            &run.id.to_string(),
            event_type,
            &run.workspace_id,
            generation,
            json!({
                "run_id": run.id.to_string(),
                "from": current.status.as_db_str(),
                "to": run.status.as_db_str(),
                "terminal_reason": run.terminal_reason,
                "generation": run.generation.get(),
            }),
        )
        .await?;
    Ok(run)
}

async fn insert_turn(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    run: &Run,
    input: &TurnInput,
) -> Result<Turn, RuntimeError> {
    let seq: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant_id)
    .bind(run.id.to_string())
    .fetch_one(&mut **tx)
    .await?;
    let id = new_id(Prefix::Turn);
    sqlx::query(
        "INSERT INTO turns (id, tenant_id, run_id, seq, input_kind, input_ref, \
         context_projection_id, status) VALUES ($1, $2, $3, $4, $5, $6, $7, 'active')",
    )
    .bind(id.to_string())
    .bind(tenant_id)
    .bind(run.id.to_string())
    .bind(seq)
    .bind(input.kind.as_db_str())
    .bind(input.reference.as_deref())
    .bind(input.context_projection_id.as_deref())
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE runs SET current_turn_id = $1 WHERE id = $2 AND tenant_id = $3")
        .bind(id.to_string())
        .bind(run.id.to_string())
        .bind(tenant_id)
        .execute(&mut **tx)
        .await?;
    load_turn_tx(tx, tenant_id, &id).await
}

async fn load_run_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Run, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM runs WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => run_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "run",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn lock_run(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Run, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM runs WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => run_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "run",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_turn_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Turn, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {TURN_COLUMNS} FROM turns WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => turn_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "turn",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn lock_turn(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Turn, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {TURN_COLUMNS} FROM turns WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => turn_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "turn",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_step_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Step, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {STEP_COLUMNS} FROM steps WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => step_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "step",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn lock_step(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Step, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {STEP_COLUMNS} FROM steps WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => step_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "step",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_attempt_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Attempt, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {ATTEMPT_COLUMNS} FROM attempts WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => attempt_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "attempt",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn lock_attempt(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Attempt, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {ATTEMPT_COLUMNS} FROM attempts WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => attempt_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "attempt",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn lock_run_for_step(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    step: &Step,
) -> Result<Run, RuntimeError> {
    let run_id: Option<String> =
        sqlx::query_scalar("SELECT t.run_id FROM turns t WHERE t.id = $1 AND t.tenant_id = $2")
            .bind(step.turn_id.to_string())
            .bind(tenant_id)
            .fetch_optional(&mut **tx)
            .await?;
    let run_id = run_id.ok_or_else(|| RuntimeError::NotFound {
        entity: "turn",
        id: step.turn_id.to_string(),
        tenant_id: tenant_id.to_string(),
    })?;
    let run_id = CanonicalId::parse_typed(&run_id, Prefix::Run)?;
    lock_run(tx, tenant_id, &run_id).await
}

fn run_from_row(row: &PgRow) -> Result<Run, RuntimeError> {
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
        current_turn_id: optional_id(row, "current_turn_id", Prefix::Turn)?,
        budget_snapshot: row.try_get("budget_snapshot")?,
        terminal_reason: row.try_get("terminal_reason")?,
        execution_target_id: row.try_get("execution_target_id")?,
        started_at: row.try_get("started_at")?,
        ended_at: row.try_get("ended_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn turn_from_row(row: &PgRow) -> Result<Turn, RuntimeError> {
    Ok(Turn {
        id: parse_id(row, "id", Prefix::Turn)?,
        tenant_id: row.try_get("tenant_id")?,
        run_id: parse_id(row, "run_id", Prefix::Run)?,
        seq: row.try_get("seq")?,
        input_kind: RunTriggerKind::from_db_str(row.try_get("input_kind")?)?,
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

fn step_from_row(row: &PgRow) -> Result<Step, RuntimeError> {
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

fn attempt_from_row(row: &PgRow) -> Result<Attempt, RuntimeError> {
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

fn generation_from_i64(value: i64) -> Result<Generation, RuntimeError> {
    let value = u64::try_from(value)
        .map_err(|_| RuntimeError::InvalidArgument(format!("negative generation {value}")))?;
    Generation::new(value).map_err(|error| RuntimeError::InvalidArgument(error.to_string()))
}

fn parse_id(row: &PgRow, column: &str, prefix: Prefix) -> Result<CanonicalId, RuntimeError> {
    let value: String = row.try_get(column)?;
    CanonicalId::parse_typed(&value, prefix)
        .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))
}

fn optional_id(
    row: &PgRow,
    column: &str,
    prefix: Prefix,
) -> Result<Option<CanonicalId>, RuntimeError> {
    let value: Option<String> = row.try_get(column)?;
    value
        .map(|value| {
            CanonicalId::parse_typed(&value, prefix)
                .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))
        })
        .transpose()
}

impl From<ProtocolStateError> for RuntimeError {
    fn from(error: ProtocolStateError) -> Self {
        match error {
            ProtocolStateError::NotFound(run_id) => RuntimeError::NotFound {
                entity: "protocol_state",
                id: run_id,
                tenant_id: String::new(),
            },
            other => RuntimeError::ProtocolState(other.to_string()),
        }
    }
}
