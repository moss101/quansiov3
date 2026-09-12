//! The recovery service: rebuild a run's position from durable state (RUN-009).
//!
//! [`Recoverer::recover_run`] reads the durable view, derives the safe action and applies what
//! recovery owns — honouring a pre-crash cancellation, refusing to resume until an uncertain
//! effect is reconciled — and hands the caller a report of exactly what it did and what it
//! requires. It never performs an external effect and never dispatches a tool call: reconciliation
//! is applied from evidence the caller supplies, because only the class's own check (or a human)
//! can know whether an uncertain action landed.

use async_trait::async_trait;
use quansio_core::CanonicalId;
use serde_json::json;

use super::plan::{fence, plan_from, DurableState, SafeAction};
use super::RecoveryError;
use crate::effects::{EffectLedger, EffectRecord, EffectStatus, ReconciliationEvidence};
use crate::runtime::protocol_state::ProtocolState;
use crate::runtime::state_machine::{Run, RunStatus, RuntimeError, RuntimeIdentity, RuntimeStore};

/// What recovery did, or requires, for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryResolution {
    /// The run was already terminal.
    Terminal,
    /// A cancellation requested before the crash was honoured.
    Cancelled,
    /// An uncertain effect must be reconciled before the run may continue.
    ReconciliationRequired {
        /// EffectRecord awaiting reconciliation.
        effect_id: String,
        /// The class's reconciliation strategy from `config/effects.yaml`.
        strategy: String,
    },
    /// The run is parked and only its own resolution releases it.
    Waiting {
        /// `WAITING_*` state the run holds.
        state: String,
        /// Key a matching resolution must carry.
        key: String,
    },
    /// The run holds no pending work and may continue at its next turn.
    Resumable,
}

/// What recovery observed and did for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Run that was recovered.
    pub run_id: String,
    /// Generation recovery observed, the fence for anything stale.
    pub generation: u64,
    /// The safe action the durable state implied.
    pub action: SafeAction,
    /// What recovery did or requires.
    pub resolution: RecoveryResolution,
    /// Attempts whose generation is behind the run's, i.e. output that must be discarded.
    pub stale_attempts: usize,
}

impl RecoveryReport {
    /// Whether the report requires a caller action before the run continues.
    #[must_use]
    pub fn requires_reconciliation(&self) -> bool {
        matches!(
            self.resolution,
            RecoveryResolution::ReconciliationRequired { .. }
        )
    }
}

/// Recovery over the canonical runtime stores.
#[derive(Clone)]
pub struct Recoverer {
    pool: sqlx::PgPool,
    identity: RuntimeIdentity,
    runs: RuntimeStore,
    ledger: EffectLedger,
}

impl Recoverer {
    /// Build the recoverer over the runtime's own stores.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the tenant is not canonical.
    pub fn new(pool: sqlx::PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        Ok(Self {
            runs: RuntimeStore::new(pool.clone(), identity.clone())?,
            ledger: EffectLedger::new(pool.clone(), identity.clone())
                .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?,
            pool,
            identity,
        })
    }

    /// The event identity recovery stamps on its transitions.
    #[must_use]
    pub fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    /// The Effect Ledger recovery reads.
    #[must_use]
    pub fn ledger(&self) -> &EffectLedger {
        &self.ledger
    }

    /// Recover one run from its durable state.
    ///
    /// # Errors
    /// Returns [`RecoveryError::NotFound`] when the run is unknown and the store's refusal when a
    /// transition cannot be applied.
    pub async fn recover_run(&self, run_id: &CanonicalId) -> Result<RecoveryReport, RecoveryError> {
        let run = self
            .runs
            .load_run(run_id)
            .await
            .map_err(|error| match error {
                RuntimeError::NotFound { .. } => RecoveryError::NotFound {
                    id: run_id.to_string(),
                },
                other => RecoveryError::Runtime(other),
            })?;
        let state = self.durable_state(&run).await?;
        let action = plan_from(&state);
        let resolution = self.apply(&run, &action, &state).await?;
        let stale_attempts = self.stale_attempts(run_id, run.generation.get()).await?;
        Ok(RecoveryReport {
            run_id: run.id.to_string(),
            generation: run.generation.get(),
            action,
            resolution,
            stale_attempts,
        })
    }

    /// Recover every non-terminal run in the tenant, oldest first.
    ///
    /// # Errors
    /// Returns a database error when the run list cannot be read.
    pub async fn scan(&self, limit: i64) -> Result<Vec<RecoveryReport>, RecoveryError> {
        let mut tx = self.pool.begin().await?;
        crate::control::schema::set_tenant_context(&mut tx, &self.identity.tenant_id)
            .await
            .map_err(|error| RecoveryError::Database(sqlx::Error::Protocol(error.to_string())))?;
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM runs WHERE tenant_id = $1 \
             AND status NOT IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'BLOCKED_UNRECOVERABLE') \
             ORDER BY created_at LIMIT $2",
        )
        .bind(&self.identity.tenant_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let mut reports = Vec::with_capacity(rows.len());
        for id in rows {
            let run_id = CanonicalId::parse_typed(&id, quansio_core::Prefix::Run)
                .map_err(|_| RecoveryError::NotFound { id: id.clone() })?;
            reports.push(self.recover_run(&run_id).await?);
        }
        Ok(reports)
    }

    /// Apply evidence to an uncertain effect, settling it without dispatching anything.
    ///
    /// # Errors
    /// Returns the ledger's refusal when the effect is not awaiting reconciliation.
    pub async fn reconcile_effect(
        &self,
        effect_id: &str,
        evidence: ReconciliationEvidence,
    ) -> Result<EffectRecord, RecoveryError> {
        Ok(self.ledger.reconcile(effect_id, evidence).await?)
    }

    /// Refuse an actor whose generation is not the run's current one.
    ///
    /// # Errors
    /// Returns [`RecoveryError::StaleGeneration`] when the actor is stale, and
    /// [`RecoveryError::NotFound`] when the run is unknown.
    pub async fn fence_worker(
        &self,
        run_id: &CanonicalId,
        observed_generation: u64,
    ) -> Result<(), RecoveryError> {
        let run = self
            .runs
            .load_run(run_id)
            .await
            .map_err(|error| match error {
                RuntimeError::NotFound { .. } => RecoveryError::NotFound {
                    id: run_id.to_string(),
                },
                other => RecoveryError::Runtime(other),
            })?;
        fence(observed_generation, run.generation.get())
    }

    /// Build the durable view recovery decides from.
    async fn durable_state(&self, run: &Run) -> Result<DurableState, RecoveryError> {
        let protocol = self.runs.load_protocol_state(&run.id).await?;
        let unsettled = self
            .ledger
            .list_unsettled()
            .await?
            .into_iter()
            .find(|record| record.run_id.as_deref() == Some(&run.id.to_string()));
        let state = protocol
            .unwrap_or_else(|| ProtocolState::new(run.id.to_string(), run.generation.get() as i64));
        Ok(DurableState {
            run_status: run.status.as_db_str().to_string(),
            cancellation_requested: state.cancellation_requested,
            pending_approvals: state.pending_approvals.clone(),
            open_questions: state.open_questions.clone(),
            child_agent_threads: state.child_agent_threads.clone(),
            parked_on_external: matches!(
                run.status,
                RunStatus::WaitingTimer | RunStatus::WaitingEvent | RunStatus::WaitingTakeover
            ),
            unsettled_effect_id: unsettled.as_ref().map(|record| record.id.clone()),
            unsettled_tool_call_id: unsettled
                .as_ref()
                .and_then(|record| record.tool_call_id.clone()),
        })
    }

    /// Apply the safe action.
    async fn apply(
        &self,
        run: &Run,
        action: &SafeAction,
        state: &DurableState,
    ) -> Result<RecoveryResolution, RecoveryError> {
        match action {
            SafeAction::Terminal => Ok(RecoveryResolution::Terminal),
            SafeAction::Cancel => {
                let cancelled = self.runs.cancel(&run.id, run.generation).await?;
                Ok(if cancelled.status == RunStatus::Cancelled {
                    RecoveryResolution::Cancelled
                } else {
                    RecoveryResolution::Terminal
                })
            }
            SafeAction::ReconcileEffect { effect_id, .. } => {
                let strategy = self
                    .ledger
                    .load(effect_id)
                    .await
                    .map(|record| record.effect_class.as_str().to_string())
                    .unwrap_or_default();
                Ok(RecoveryResolution::ReconciliationRequired {
                    effect_id: effect_id.clone(),
                    strategy: self.reconciliation_strategy(&strategy),
                })
            }
            SafeAction::Wait { state, key } => Ok(RecoveryResolution::Waiting {
                state: state.clone(),
                key: key.clone(),
            }),
            SafeAction::Resume => {
                let _ = state;
                Ok(RecoveryResolution::Resumable)
            }
        }
    }

    /// The configured reconciliation strategy for an effect class.
    fn reconciliation_strategy(&self, effect_class: &str) -> String {
        self.ledger
            .taxonomy()
            .get(effect_class)
            .map(|entry| format!("{:?}", entry.reconciliation).to_lowercase())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Count attempts whose generation is behind the run's.
    async fn stale_attempts(
        &self,
        run_id: &CanonicalId,
        generation: u64,
    ) -> Result<usize, RecoveryError> {
        let mut tx = self.pool.begin().await?;
        crate::control::schema::set_tenant_context(&mut tx, &self.identity.tenant_id)
            .await
            .map_err(|error| RecoveryError::Database(sqlx::Error::Protocol(error.to_string())))?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM attempts a JOIN steps s ON s.id = a.step_id \
             JOIN turns t ON t.id = s.turn_id \
             WHERE t.run_id = $1 AND a.tenant_id = $2 AND a.generation <> $3",
        )
        .bind(run_id.to_string())
        .bind(&self.identity.tenant_id)
        .bind(i64::try_from(generation).unwrap_or(1))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Record a fence decision as an event so the discarded work is visible in the trail.
    ///
    /// # Errors
    /// Returns an event-store refusal when the event cannot be committed.
    pub async fn record_fence(
        &self,
        run_id: &CanonicalId,
        observed_generation: u64,
        current_generation: u64,
    ) -> Result<(), RecoveryError> {
        use quansio_events::event_type::EventType;
        use quansio_events::{EventDraft, EventStore};
        let aggregate_id = run_id.to_string();
        let mut tx = self.pool.begin().await?;
        crate::control::schema::set_tenant_context(&mut tx, &self.identity.tenant_id)
            .await
            .map_err(|error| RecoveryError::Database(sqlx::Error::Protocol(error.to_string())))?;
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(aggregate_version), 0) + 1 FROM runtime_events \
             WHERE tenant_id = $1 AND aggregate_type = 'run' AND aggregate_id = $2",
        )
        .bind(&self.identity.tenant_id)
        .bind(&aggregate_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let draft = EventDraft::new(
            "run",
            &aggregate_id,
            u64::try_from(next).unwrap_or(1),
            EventType::parse("run.stale_worker_fenced")
                .map_err(|error| RecoveryError::Runtime(RuntimeError::Event(error)))?,
            self.identity.correlation_id,
            self.identity.actor.clone(),
        )
        .with_payload(json!({
            "run_id": aggregate_id,
            "observed_generation": observed_generation,
            "current_generation": current_generation,
        }));
        EventStore::new(self.pool.clone())
            .commit_mutation_tx(&self.identity.tenant_id, move |_tx, batch| {
                Box::pin(async move {
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await
            .map_err(|error| RecoveryError::Runtime(RuntimeError::Event(error)))?;
        Ok(())
    }
}

/// The recovery port, so a composition root can drive recovery without the concrete store.
#[async_trait]
pub trait RecoveryPort: Send + Sync {
    /// Recover one run.
    ///
    /// # Errors
    /// Returns the recovery refusal when the run cannot be recovered.
    async fn recover(&self, run_id: &CanonicalId) -> Result<RecoveryReport, RecoveryError>;
}

#[async_trait]
impl RecoveryPort for Recoverer {
    async fn recover(&self, run_id: &CanonicalId) -> Result<RecoveryReport, RecoveryError> {
        self.recover_run(run_id).await
    }
}

/// Whether an effect record is awaiting reconciliation.
#[must_use]
pub fn awaits_reconciliation(record: &EffectRecord) -> bool {
    record.status == EffectStatus::OutcomeUnknown
}
