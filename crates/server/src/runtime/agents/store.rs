//! Durable AgentThread rows, mailbox, delegation, handoff and join (RUN-002).
//!
//! Every method is one PostgreSQL transaction: the row changes and the `agent.*` (and,
//! for a run released by a join, `run.*`) RuntimeEvents are committed together through
//! [`EventStore::commit_mutation_tx`], which refuses a transaction that staged no event.
//! A rejected lifecycle transition, a widening delegation, a mismatched handoff or a
//! stale generation rolls back, so state and event log never disagree.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use quansio_core::{CanonicalId, Generation, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::control::schema;

use super::{
    agent_event_name, check_lifecycle_policy, fence, AgentKind, AgentThreadStatus,
    TEAMMATE_CANNOT_JOIN,
};
use crate::runtime::state_machine::{RuntimeError, RuntimeIdentity};

/// Repository path used to attribute a rejected agent mutation.
pub const AGENTS_OWNER: &str = "crates/server/src/runtime/agents";

/// Boxed future returned by an agent mutation closure.
type BoxAgentFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RuntimeError>> + Send + 'a>>;

const THREAD_COLUMNS: &str = "id, tenant_id, workspace_id, agent_kind, definition_id, parent_id, \
     work_node_id, capability_projection_id, generation, status, mailbox_cursor, \
     execution_target_id, budget_id, suspended_reason, handoff_to_agent_thread_id, handoff_at, \
     created_at, updated_at";

const EDGE_COLUMNS: &str = "id, tenant_id, parent_agent_thread_id, child_agent_thread_id, \
     work_node_id, delegation_capability_id, delegated_at, joined_at";

const MAILBOX_COLUMNS: &str =
    "agent_thread_id, tenant_id, workspace_id, seq, message_id, payload, delivered_at";

const HANDOFF_COLUMNS: &str = "id, tenant_id, workspace_id, from_agent_thread_id, \
     to_agent_thread_id, run_id, run_generation, work_node_id, mailbox_cursor, evidence_ids, \
     pending_child_thread_ids, conversation_ref, status, completed_at, created_at, updated_at";

const JOIN_COLUMNS: &str = "child_agent_thread_id, tenant_id, workspace_id, \
     parent_agent_thread_id, edge_id, child_status, evidence_ids, artifact_ids, outcome, joined_at";

/// Fields required to create an [`AgentThread`] (DOMAIN.md §5.1).
#[derive(Debug, Clone)]
pub struct NewAgentThread {
    /// Workspace the thread participates in.
    pub workspace_id: String,
    /// Persistent teammate or ephemeral worker.
    pub agent_kind: AgentKind,
    /// Teammate definition, for a persistent teammate.
    pub definition_id: Option<CanonicalId>,
    /// Delegating parent thread, when spawned by delegation.
    pub parent_id: Option<CanonicalId>,
    /// WorkNode the thread participates in.
    pub work_node_id: Option<CanonicalId>,
    /// Capability projection resolved for the thread.
    pub capability_projection_id: Option<CanonicalId>,
    /// Execution target bound to the thread.
    pub execution_target_id: Option<CanonicalId>,
    /// Budget row reference.
    pub budget_id: Option<String>,
}

impl NewAgentThread {
    /// A thread with DOMAIN defaults: no lineage, projection or target.
    #[must_use]
    pub fn new(workspace_id: impl Into<String>, agent_kind: AgentKind) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            agent_kind,
            definition_id: None,
            parent_id: None,
            work_node_id: None,
            capability_projection_id: None,
            execution_target_id: None,
            budget_id: None,
        }
    }

    /// Set the WorkNode the thread serves.
    #[must_use]
    pub fn with_work_node(mut self, node: CanonicalId) -> Self {
        self.work_node_id = Some(node);
        self
    }

    /// Set the capability projection the thread runs under.
    #[must_use]
    pub fn with_capability_projection(mut self, projection: CanonicalId) -> Self {
        self.capability_projection_id = Some(projection);
        self
    }
}

/// A persisted `agent_threads` row (DOMAIN.md §5.1).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentThread {
    /// Canonical `ath_…` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Teammate or worker.
    pub agent_kind: AgentKind,
    /// Teammate definition.
    pub definition_id: Option<CanonicalId>,
    /// Delegating parent.
    pub parent_id: Option<CanonicalId>,
    /// WorkNode the thread participates in.
    pub work_node_id: Option<CanonicalId>,
    /// Capability projection.
    pub capability_projection_id: Option<CanonicalId>,
    /// Fencing generation.
    pub generation: Generation,
    /// Lifecycle state.
    pub status: AgentThreadStatus,
    /// Mailbox cursor (last acknowledged seq).
    pub mailbox_cursor: Option<String>,
    /// Execution target.
    pub execution_target_id: Option<CanonicalId>,
    /// Budget row reference.
    pub budget_id: Option<String>,
    /// Why the thread is suspended.
    pub suspended_reason: Option<String>,
    /// Handoff target.
    pub handoff_to_agent_thread_id: Option<CanonicalId>,
    /// Handoff start time.
    pub handoff_at: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// A persisted `agent_graph_edges` row: one delegation (DOMAIN.md §4.3).
///
/// The id is `age_<ULID>` per `0001_canonical_schema.sql`; `age_` is not in the DOMAIN
/// §1.1 prefix table, so it is carried as an opaque string rather than a `CanonicalId`.
#[derive(Debug, Clone, PartialEq)]
pub struct DelegationEdge {
    /// `age_<ULID>` identity.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Parent agent thread.
    pub parent_agent_thread_id: CanonicalId,
    /// Child agent thread.
    pub child_agent_thread_id: CanonicalId,
    /// WorkNode the delegation serves.
    pub work_node_id: Option<CanonicalId>,
    /// The parent's delegated capability id recorded on the edge.
    pub delegation_capability_id: Option<CanonicalId>,
    /// When the delegation was recorded.
    pub delegated_at: DateTime<Utc>,
    /// When the worker joined its parent.
    pub joined_at: Option<DateTime<Utc>>,
}

/// Request to delegate work from a parent agent thread to a new child (DOMAIN.md §4.3).
#[derive(Debug, Clone)]
pub struct NewDelegation {
    /// Child thread to create (`PROVISIONED`).
    pub child: NewAgentThread,
    /// WorkNode the delegation serves; defaults to the child's `work_node_id`.
    pub work_node_id: Option<CanonicalId>,
    /// Explicit delegated capability id; defaults to the parent's projection.
    pub delegation_capability_id: Option<CanonicalId>,
    /// Instruction delivered to the child's mailbox in the same transaction.
    pub instruction: Option<String>,
}

impl NewDelegation {
    /// A delegation that inherits the parent's capability projection.
    #[must_use]
    pub fn new(child: NewAgentThread) -> Self {
        Self {
            work_node_id: child.work_node_id,
            delegation_capability_id: None,
            instruction: None,
            child,
        }
    }

    /// Deliver an instruction to the child's mailbox with the delegation.
    #[must_use]
    pub fn with_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Set the WorkNode the delegation serves.
    #[must_use]
    pub fn with_work_node(mut self, work_node_id: CanonicalId) -> Self {
        self.work_node_id = Some(work_node_id);
        self
    }
}

/// A delegation: the created child thread and its AgentGraph edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Delegated {
    /// The child agent thread (created `PROVISIONED`).
    pub child: AgentThread,
    /// The delegation edge.
    pub edge: DelegationEdge,
}

/// Narrowing check applied to every delegation (DOMAIN.md §6.3).
///
/// RUN-005 implements this trait with the Capability Projection algebra; until then
/// [`StructuralDelegationCheck`] enforces the rule provable from the graph alone. The
/// check runs inside the delegation transaction before any insert, so a rejected
/// delegation writes nothing. The signature mirrors `quansio_graph`'s
/// `DelegationNarrowingCheck` (this crate cannot depend on `crates/graph` because that
/// crate depends on this one).
pub trait DelegationNarrowingCheck: Send + Sync {
    /// Reject a delegation whose child does not narrow the parent's authority.
    ///
    /// # Errors
    /// Returns [`RuntimeError::CapabilityWideningRejected`] when the delegation widens.
    fn check(
        &self,
        parent: &AgentThread,
        child: &NewAgentThread,
        delegated_capability_id: Option<&CanonicalId>,
    ) -> Result<(), RuntimeError>;
}

/// The structural narrowing rule shipped before RUN-005: the child records exactly the
/// parent's capability projection, and no capability is invented when the parent has none.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuralDelegationCheck;

impl DelegationNarrowingCheck for StructuralDelegationCheck {
    fn check(
        &self,
        parent: &AgentThread,
        child: &NewAgentThread,
        delegated_capability_id: Option<&CanonicalId>,
    ) -> Result<(), RuntimeError> {
        match (
            &parent.capability_projection_id,
            delegated_capability_id,
            &child.capability_projection_id,
        ) {
            (Some(parent_capability), Some(recorded), Some(child_capability))
                if recorded == parent_capability && child_capability == parent_capability =>
            {
                Ok(())
            }
            (Some(_), _, _) => Err(RuntimeError::CapabilityWideningRejected {
                detail: "child must record the parent's delegated capability id".to_string(),
            }),
            (None, None, None) => Ok(()),
            (None, _, _) => Err(RuntimeError::CapabilityWideningRejected {
                detail: "parent holds no delegation capability, so the child cannot record one"
                    .to_string(),
            }),
        }
    }
}

/// A monotonic mailbox cursor: the highest `seq` an agent thread has acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MailboxCursor(u64);

impl MailboxCursor {
    /// The cursor of a thread that has acknowledged nothing.
    pub const INITIAL: Self = Self(0);

    /// A cursor at `seq`.
    #[must_use]
    pub const fn new(seq: u64) -> Self {
        Self(seq)
    }

    /// The raw seq.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Parse a stored cursor; `None` and `""` mean [`MailboxCursor::INITIAL`].
    ///
    /// # Errors
    /// Returns [`RuntimeError::InvalidArgument`] for a non-decimal cursor.
    pub fn parse(value: Option<&str>) -> Result<Self, RuntimeError> {
        match value {
            None | Some("") => Ok(Self::INITIAL),
            Some(raw) => raw.parse::<u64>().map(Self).map_err(|_| {
                RuntimeError::InvalidArgument(format!("invalid mailbox cursor {raw:?}"))
            }),
        }
    }
}

impl std::fmt::Display for MailboxCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A durable mailbox item delivered to one AgentThread.
#[derive(Debug, Clone, PartialEq)]
pub struct MailboxItem {
    /// Receiving agent thread.
    pub agent_thread_id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Per-thread monotonic sequence.
    pub seq: i64,
    /// Durable message identity; unique per thread.
    pub message_id: String,
    /// Delivered payload.
    pub payload: Value,
    /// Delivery time.
    pub delivered_at: DateTime<Utc>,
}

/// A mailbox item to deliver.
#[derive(Debug, Clone)]
pub struct NewMailboxItem {
    /// Durable message identity; redelivering the same id is idempotent.
    pub message_id: String,
    /// Delivered payload.
    pub payload: Value,
}

impl NewMailboxItem {
    /// A mailbox item with an empty payload.
    #[must_use]
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            payload: json!({}),
        }
    }

    /// Set the delivered payload.
    #[must_use]
    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
}

/// Handoff record state (RUN-002; DOSSIER.md §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandoffStatus {
    /// Written before the source started handing over; recoverable.
    Pending,
    /// The transfer completed once.
    Completed,
    /// The source kept its work.
    RolledBack,
}

impl HandoffStatus {
    /// The stored form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Completed => "COMPLETED",
            Self::RolledBack => "ROLLED_BACK",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "PENDING" => Ok(Self::Pending),
            "COMPLETED" => Ok(Self::Completed),
            "ROLLED_BACK" => Ok(Self::RolledBack),
            other => Err(RuntimeError::unknown_state("agent_handoff", other)),
        }
    }
}

/// A durable, recoverable handoff record.
///
/// The id is `ahf_<ULID>`: like `age_`, the prefix is not in the DOMAIN §1.1 table, so it
/// is carried opaquely.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentHandoff {
    /// `ahf_<ULID>` identity.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Thread handing its work over.
    pub from_agent_thread_id: CanonicalId,
    /// Thread receiving the work.
    pub to_agent_thread_id: CanonicalId,
    /// Run whose live work moves.
    pub run_id: Option<CanonicalId>,
    /// Generation the run was at when the handoff began.
    pub run_generation: Option<Generation>,
    /// WorkNode the run executes.
    pub work_node_id: Option<CanonicalId>,
    /// Mailbox cursor transferred to the target.
    pub mailbox_cursor: Option<String>,
    /// Evidence references carried across.
    pub evidence_ids: Value,
    /// Pending child threads carried across.
    pub pending_child_thread_ids: Value,
    /// Opaque conversation-continuity reference.
    pub conversation_ref: Option<String>,
    /// Record state.
    pub status: HandoffStatus,
    /// When the handoff completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// Request to begin a handoff (DOMAIN.md §5.1 `handoff {to_agent_thread_id, at}`).
#[derive(Debug, Clone)]
pub struct NewHandoff {
    /// Thread receiving the work.
    pub to_agent_thread_id: CanonicalId,
    /// Run whose live work moves, when the source owns one.
    pub run_id: Option<CanonicalId>,
    /// WorkNode the run executes.
    pub work_node_id: Option<CanonicalId>,
    /// Evidence references carried across.
    pub evidence_ids: Vec<String>,
    /// Opaque conversation-continuity reference.
    pub conversation_ref: Option<String>,
}

impl NewHandoff {
    /// A handoff to `to_agent_thread_id`.
    #[must_use]
    pub fn new(to_agent_thread_id: CanonicalId) -> Self {
        Self {
            to_agent_thread_id,
            run_id: None,
            work_node_id: None,
            evidence_ids: Vec::new(),
            conversation_ref: None,
        }
    }

    /// Hand a specific run's live work over.
    #[must_use]
    pub fn with_run(mut self, run_id: CanonicalId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// Carry evidence references across the handoff.
    #[must_use]
    pub fn with_evidence(mut self, evidence_ids: Vec<String>) -> Self {
        self.evidence_ids = evidence_ids;
        self
    }

    /// Carry conversation continuity across the handoff.
    #[must_use]
    pub fn with_conversation_ref(mut self, reference: impl Into<String>) -> Self {
        self.conversation_ref = Some(reference.into());
        self
    }
}

/// Result of completing a handoff.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedHandoff {
    /// The completed record.
    pub handoff: AgentHandoff,
    /// Source thread (`HANDED_OFF`).
    pub source: AgentThread,
    /// Target thread (`ACTIVE`).
    pub target: AgentThread,
    /// Run that moved, when the handoff named one.
    pub run_id: Option<CanonicalId>,
    /// Whether this call replayed an already-completed handoff.
    pub duplicate: bool,
}

/// The once-only merge of a worker's outcome into its parent (DOMAIN.md §5.1, §4.3).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentJoin {
    /// Worker that joined.
    pub child_agent_thread_id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Parent the outcome merged into.
    pub parent_agent_thread_id: CanonicalId,
    /// Delegation edge the join closes.
    pub edge_id: String,
    /// Terminal status the worker reported.
    pub child_status: String,
    /// Evidence ids merged into the parent's lineage.
    pub evidence_ids: Value,
    /// Artifact ids produced by the worker.
    pub artifact_ids: Value,
    /// Free-form outcome payload.
    pub outcome: Value,
    /// When the merge was committed.
    pub joined_at: DateTime<Utc>,
}

/// Request to join a worker's outcome into its parent (DOMAIN.md §5.1, §5.6).
#[derive(Debug, Clone)]
pub struct JoinRequest {
    /// Worker joining.
    pub child_agent_thread_id: CanonicalId,
    /// Generation the caller fences the child with.
    pub child_generation: Generation,
    /// Status the worker reports for the finished work.
    pub child_status: String,
    /// Evidence ids produced by the worker.
    pub evidence_ids: Vec<String>,
    /// Artifact ids produced by the worker.
    pub artifact_ids: Vec<String>,
    /// Free-form outcome payload.
    pub outcome: Value,
    /// Parent run to release from `WAITING_CHILD` when it was parked on this worker.
    pub parent_run_id: Option<CanonicalId>,
    /// Generation the caller fences the parent run with.
    pub parent_run_generation: Option<Generation>,
}

impl JoinRequest {
    /// A join of `child_agent_thread_id` with no outcome payload.
    #[must_use]
    pub fn new(child_agent_thread_id: CanonicalId, child_generation: Generation) -> Self {
        Self {
            child_agent_thread_id,
            child_generation,
            child_status: "SUCCEEDED".to_string(),
            evidence_ids: Vec::new(),
            artifact_ids: Vec::new(),
            outcome: json!({}),
            parent_run_id: None,
            parent_run_generation: None,
        }
    }

    /// Record the ids the merge carries into the parent's lineage.
    #[must_use]
    pub fn with_lineage(
        mut self,
        evidence_ids: Vec<String>,
        artifact_ids: Vec<String>,
        outcome: Value,
    ) -> Self {
        self.evidence_ids = evidence_ids;
        self.artifact_ids = artifact_ids;
        self.outcome = outcome;
        self
    }

    /// Release the parent run from `WAITING_CHILD` once this worker and its siblings joined.
    #[must_use]
    pub fn with_parent_run(mut self, run_id: CanonicalId, generation: Generation) -> Self {
        self.parent_run_id = Some(run_id);
        self.parent_run_generation = Some(generation);
        self
    }
}

/// Result of a join.
#[derive(Debug, Clone, PartialEq)]
pub struct Joined {
    /// Worker (now `JOINED`).
    pub child: AgentThread,
    /// Delegation edge the join closed.
    pub edge: DelegationEdge,
    /// The lineage merge.
    pub merge: AgentJoin,
    /// Whether the parent run was released from `WAITING_CHILD`.
    pub parent_run_resumed: bool,
    /// Whether this call replayed an existing merge.
    pub duplicate: bool,
}

/// The AgentThread / mailbox / handoff / join store (RUN-002).
#[derive(Debug, Clone)]
pub struct AgentStore {
    events: EventStore,
    identity: RuntimeIdentity,
}

impl AgentStore {
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

    async fn commit<T, F>(&self, mutation: F) -> Result<T, RuntimeError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxAgentFuture<'a, T>,
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
                                owner: AGENTS_OWNER,
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
                        owner: AGENTS_OWNER,
                        message: "transaction rejected without a recorded reason".to_string(),
                    })
                }))
            }
            Err(error) => Err(RuntimeError::Event(error)),
        }
    }

    // ------------------------------------------------------------------ lifecycle

    /// Create an AgentThread in `PROVISIONED` and emit `agent.thread_provisioned`.
    ///
    /// # Errors
    /// Returns a database error when the workspace, parent or WorkNode is not visible in
    /// this tenant.
    pub async fn create_thread(&self, thread: NewAgentThread) -> Result<AgentThread, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let created = insert_thread(tx, &tenant_id, &thread).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &created.id.to_string(),
                        "agent.thread_provisioned",
                        &created.workspace_id,
                        created.generation,
                        json!({
                            "agent_thread_id": created.id.to_string(),
                            "workspace_id": created.workspace_id,
                            "agent_kind": created.agent_kind.as_db_str(),
                            "parent_id": created.parent_id.as_ref().map(ToString::to_string),
                            "work_node_id": created.work_node_id.as_ref().map(ToString::to_string),
                            "status": created.status.as_db_str(),
                            "generation": created.generation.get(),
                        }),
                    )
                    .await?;
                Ok(created)
            })
        })
        .await
    }

    /// Apply one legal AgentThread transition and emit its `agent.*` event.
    ///
    /// `JOINING` and `JOINED` are entered by [`AgentStore::join_worker`], which performs
    /// the `ACTIVE → JOINING → JOINED` chain and the lineage merge in one transaction.
    ///
    /// # Errors
    /// Returns [`RuntimeError::FencedStaleGeneration`] for a stale generation,
    /// [`RuntimeError::IllegalTransition`] when DOMAIN.md §5.1 does not allow the pair,
    /// [`RuntimeError::LifecyclePolicy`] when the kind's policy forbids it, and
    /// [`RuntimeError::NotFound`] when the thread is not visible in this tenant. Nothing
    /// is written on error.
    pub async fn transition(
        &self,
        id: &CanonicalId,
        generation: Generation,
        to: AgentThreadStatus,
    ) -> Result<AgentThread, RuntimeError> {
        self.transition_with_reason(id, generation, to, None).await
    }

    /// Suspend an `ACTIVE` thread (`agent.suspended`).
    ///
    /// # Errors
    /// See [`AgentStore::transition`].
    pub async fn suspend(
        &self,
        id: &CanonicalId,
        generation: Generation,
        reason: impl Into<String>,
    ) -> Result<AgentThread, RuntimeError> {
        self.transition_with_reason(
            id,
            generation,
            AgentThreadStatus::Suspended,
            Some(reason.into()),
        )
        .await
    }

    /// Activate a `PROVISIONED` or `SUSPENDED` thread (`agent.activated`).
    ///
    /// # Errors
    /// See [`AgentStore::transition`].
    pub async fn activate(
        &self,
        id: &CanonicalId,
        generation: Generation,
    ) -> Result<AgentThread, RuntimeError> {
        self.transition(id, generation, AgentThreadStatus::Active)
            .await
    }

    /// Terminate a thread subject to the kind's policy (`agent.terminated`).
    ///
    /// A worker must be `JOINED`; a teammate may terminate from any non-terminal state.
    ///
    /// # Errors
    /// See [`AgentStore::transition`].
    pub async fn terminate(
        &self,
        id: &CanonicalId,
        generation: Generation,
    ) -> Result<AgentThread, RuntimeError> {
        self.transition(id, generation, AgentThreadStatus::Terminated)
            .await
    }

    async fn transition_with_reason(
        &self,
        id: &CanonicalId,
        generation: Generation,
        to: AgentThreadStatus,
        reason: Option<String>,
    ) -> Result<AgentThread, RuntimeError> {
        if matches!(
            to,
            AgentThreadStatus::Joining | AgentThreadStatus::Joined | AgentThreadStatus::Provisioned
        ) {
            return Err(RuntimeError::InvalidArgument(format!(
                "{} is applied by its owning operation, not transition",
                to.as_db_str()
            )));
        }
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let id = *id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let current = lock_thread(tx, &tenant_id, &id).await?;
                fence(current.generation, generation)?;
                check_transition(current.agent_kind, current.status, to)?;
                let updated = sqlx::query(
                    "UPDATE agent_threads SET status = $1, \
                     suspended_reason = CASE WHEN $1 = 'SUSPENDED' THEN $2 WHEN $1 = 'ACTIVE' THEN NULL \
                                            ELSE suspended_reason END \
                     WHERE id = $3 AND tenant_id = $4 AND status = $5",
                )
                .bind(to.as_db_str())
                .bind(reason.as_deref())
                .bind(id.to_string())
                .bind(&tenant_id)
                .bind(current.status.as_db_str())
                .execute(&mut **tx)
                .await?;
                if updated.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: id.to_string(),
                    });
                }
                let thread = load_thread_tx(tx, &tenant_id, &id).await?;
                let event_type = agent_event_name(to).ok_or_else(|| {
                    RuntimeError::InvalidArgument(format!(
                        "DOMAIN.md §9.2 names no agent.* event for {}",
                        to.as_db_str()
                    ))
                })?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &thread.id.to_string(),
                                                event_type,
                        &thread.workspace_id,
                        thread.generation,
                        json!({
                            "agent_thread_id": thread.id.to_string(),
                            "from": current.status.as_db_str(),
                            "to": thread.status.as_db_str(),
                            "generation": thread.generation.get(),
                        }),
                    )
                    .await?;
                Ok(thread)
            })
        })
        .await
    }

    /// Terminate a thread the policy allows to end.
    ///
    /// Provided for callers that need the DOMAIN.md §5.1 "any → TERMINATED" rule for a
    /// teammate without naming the target state themselves.
    ///
    /// # Errors
    /// See [`AgentStore::terminate`].
    pub async fn cancel_thread(
        &self,
        id: &CanonicalId,
        generation: Generation,
    ) -> Result<AgentThread, RuntimeError> {
        self.terminate(id, generation).await
    }

    // ------------------------------------------------------------------ delegation

    /// Delegate to a new child thread using the structural narrowing rule.
    ///
    /// # Errors
    /// See [`AgentStore::delegate_with`].
    pub async fn delegate(
        &self,
        parent_id: &CanonicalId,
        request: NewDelegation,
    ) -> Result<Delegated, RuntimeError> {
        self.delegate_with(Arc::new(StructuralDelegationCheck), parent_id, request)
            .await
    }

    /// Delegate with an explicit [`DelegationNarrowingCheck`] (the RUN-005 seam).
    ///
    /// The check runs before any insert, so a widening delegation commits nothing.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the parent is not visible,
    /// [`RuntimeError::WorkspaceMismatch`]-style [`RuntimeError::InvalidArgument`] when the
    /// child belongs to another workspace, and
    /// [`RuntimeError::CapabilityWideningRejected`] when the check rejects the delegation.
    pub async fn delegate_with(
        &self,
        check: Arc<dyn DelegationNarrowingCheck>,
        parent_id: &CanonicalId,
        request: NewDelegation,
    ) -> Result<Delegated, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let parent_id = *parent_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                expect_prefix(&parent_id, Prefix::AgentThread)?;
                let parent = lock_thread(tx, &tenant_id, &parent_id).await?;
                if parent.workspace_id != request.child.workspace_id {
                    return Err(RuntimeError::InvalidArgument(format!(
                        "child workspace {} differs from parent workspace {}",
                        request.child.workspace_id, parent.workspace_id
                    )));
                }
                let resolved = request
                    .delegation_capability_id
                    .or(parent.capability_projection_id);
                if let Some(capability) = &resolved {
                    expect_prefix(capability, Prefix::CapabilityProjection)?;
                }
                check.check(&parent, &request.child, resolved.as_ref())?;
                let mut child = request.child.clone();
                child.parent_id = Some(parent.id);
                let created = insert_thread(tx, &tenant_id, &child).await?;
                let edge_id = format!("age_{}", new_ulid());
                let work_node_id = request.work_node_id.or(created.work_node_id);
                sqlx::query(
                    "INSERT INTO agent_graph_edges (id, tenant_id, parent_agent_thread_id, \
                     child_agent_thread_id, work_node_id, delegation_capability_id) \
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(&edge_id)
                .bind(&tenant_id)
                .bind(parent.id.to_string())
                .bind(created.id.to_string())
                .bind(work_node_id.as_ref().map(ToString::to_string))
                .bind(resolved.as_ref().map(ToString::to_string))
                .execute(&mut **tx)
                .await?;
                let edge = load_edge_tx(tx, &tenant_id, &edge_id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &created.id.to_string(),
                        "agent.thread_provisioned",
                        &created.workspace_id,
                        created.generation,
                        json!({
                            "agent_thread_id": created.id.to_string(),
                            "parent_id": created.parent_id.as_ref().map(ToString::to_string),
                            "work_node_id": created.work_node_id.as_ref().map(ToString::to_string),
                            "status": created.status.as_db_str(),
                            "generation": created.generation.get(),
                        }),
                    )
                    .await?;
                if let Some(instruction) = &request.instruction {
                    let item = insert_mailbox_item(
                        tx,
                        &tenant_id,
                        &created,
                        &NewMailboxItem::new(format!("msg_{}", new_ulid()))
                            .with_payload(json!({ "instruction": instruction })),
                    )
                    .await?;
                    emit_mailbox_delivered(tx, batch, &mut publisher, &created, &item).await?;
                }
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &parent.id.to_string(),
                        "agent.delegated",
                        &parent.workspace_id,
                        parent.generation,
                        json!({
                            "parent_agent_thread_id": parent.id.to_string(),
                            "child_agent_thread_id": created.id.to_string(),
                            "delegation_id": edge.id,
                            "work_node_id": edge.work_node_id.as_ref().map(ToString::to_string),
                            "delegation_capability_id": edge
                                .delegation_capability_id
                                .as_ref()
                                .map(ToString::to_string),
                        }),
                    )
                    .await?;
                Ok(Delegated {
                    child: created,
                    edge,
                })
            })
        })
        .await
    }

    // ------------------------------------------------------------------ mailbox

    /// Deliver one durable mailbox item and emit `agent.mailbox_delivered`.
    ///
    /// Delivering the same `message_id` again to the same thread returns the existing
    /// item and emits nothing, so redelivery is idempotent.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the thread is not visible in this tenant.
    pub async fn deliver(
        &self,
        agent_thread_id: &CanonicalId,
        item: NewMailboxItem,
    ) -> Result<MailboxItem, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let existing = self.load_thread(agent_thread_id).await?;
        if let Some(found) = self
            .find_mailbox_item(&existing.id, &item.message_id)
            .await?
        {
            return Ok(found);
        }
        let identity = self.identity.clone();
        let agent_thread_id = *agent_thread_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let thread = lock_thread(tx, &tenant_id, &agent_thread_id).await?;
                if thread.status.is_terminal() {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "agent_thread",
                        from: thread.status.as_db_str().to_string(),
                        to: "mailbox delivery".to_string(),
                    });
                }
                let item = insert_mailbox_item(tx, &tenant_id, &thread, &item).await?;
                emit_mailbox_delivered(tx, batch, &mut publisher, &thread, &item).await?;
                Ok(item)
            })
        })
        .await
    }

    /// Items delivered after `cursor`, in order; unacknowledged items are returned again.
    ///
    /// `poll` is a pure read, so a re-poll after a pool drop returns exactly the same
    /// unacknowledged items and never a duplicate.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the thread is not visible in this tenant.
    pub async fn poll(
        &self,
        agent_thread_id: &CanonicalId,
        cursor: MailboxCursor,
    ) -> Result<Vec<MailboxItem>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let thread = self.load_thread(agent_thread_id).await?;
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {MAILBOX_COLUMNS} FROM agent_mailbox_items \
             WHERE tenant_id = $1 AND agent_thread_id = $2 AND seq > $3 ORDER BY seq"
        ))
        .bind(&tenant_id)
        .bind(thread.id.to_string())
        .bind(cursor.get() as i64)
        .fetch_all(&mut *tx)
        .await?;
        let items = rows
            .iter()
            .map(mailbox_from_row)
            .collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(items)
    }

    /// Advance a thread's mailbox cursor, emitting `agent.mailbox_cursor_advanced`.
    ///
    /// The cursor is monotonic: advancing to the same or an earlier value returns the
    /// thread unchanged and emits nothing, so a retried acknowledgement is idempotent.
    ///
    /// # Errors
    /// Returns [`RuntimeError::FencedStaleGeneration`] for a stale generation and
    /// [`RuntimeError::InvalidArgument`] when the cursor is past everything delivered.
    pub async fn advance_cursor(
        &self,
        agent_thread_id: &CanonicalId,
        generation: Generation,
        cursor: MailboxCursor,
    ) -> Result<AgentThread, RuntimeError> {
        let current = self.load_thread(agent_thread_id).await?;
        fence(current.generation, generation)?;
        let current_cursor = MailboxCursor::parse(current.mailbox_cursor.as_deref())?;
        if cursor <= current_cursor {
            return Ok(current);
        }
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let agent_thread_id = *agent_thread_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let thread = lock_thread(tx, &tenant_id, &agent_thread_id).await?;
                fence(thread.generation, generation)?;
                let current_cursor = MailboxCursor::parse(thread.mailbox_cursor.as_deref())?;
                if cursor <= current_cursor {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: thread.id.to_string(),
                    });
                }
                let delivered: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(seq), 0) FROM agent_mailbox_items \
                     WHERE tenant_id = $1 AND agent_thread_id = $2",
                )
                .bind(&tenant_id)
                .bind(thread.id.to_string())
                .fetch_one(&mut **tx)
                .await?;
                if cursor.get() as i64 > delivered {
                    return Err(RuntimeError::InvalidArgument(format!(
                        "cursor {cursor} is past the last delivered seq {delivered}"
                    )));
                }
                sqlx::query(
                    "UPDATE agent_threads SET mailbox_cursor = $1 \
                     WHERE id = $2 AND tenant_id = $3 AND mailbox_cursor IS NOT DISTINCT FROM $4",
                )
                .bind(cursor.to_string())
                .bind(thread.id.to_string())
                .bind(&tenant_id)
                .bind(thread.mailbox_cursor.as_deref())
                .execute(&mut **tx)
                .await?;
                let updated = load_thread_tx(tx, &tenant_id, &thread.id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &updated.id.to_string(),
                        "agent.mailbox_cursor_advanced",
                        &updated.workspace_id,
                        updated.generation,
                        json!({
                            "agent_thread_id": updated.id.to_string(),
                            "cursor": cursor.get(),
                            "previous_cursor": current_cursor.get(),
                        }),
                    )
                    .await?;
                Ok(updated)
            })
        })
        .await
    }

    // ------------------------------------------------------------------ handoff

    /// Begin a handoff: the durable intent plus `agent.handoff_started`.
    ///
    /// The record is written `PENDING` in the same transaction that moves the source to
    /// `HANDING_OFF`, so a crash before [`AgentStore::complete_handoff`] leaves enough
    /// state for a restarted runtime to finish or roll back.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] unless the source is `ACTIVE`,
    /// [`RuntimeError::InvalidArgument`] when the target is the source or already holds a
    /// mailbox cursor, and [`RuntimeError::NotFound`] for an invisible thread or run.
    pub async fn begin_handoff(
        &self,
        from_agent_thread_id: &CanonicalId,
        generation: Generation,
        request: NewHandoff,
    ) -> Result<AgentHandoff, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let from_agent_thread_id = *from_agent_thread_id;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let source = lock_thread(tx, &tenant_id, &from_agent_thread_id).await?;
                fence(source.generation, generation)?;
                check_transition(source.agent_kind, source.status, AgentThreadStatus::HandingOff)?;
                if request.to_agent_thread_id == source.id {
                    return Err(RuntimeError::InvalidArgument(
                        "a thread cannot hand its work to itself".to_string(),
                    ));
                }
                let target = lock_thread(tx, &tenant_id, &request.to_agent_thread_id).await?;
                if target.workspace_id != source.workspace_id {
                    return Err(RuntimeError::InvalidArgument(
                        "handoff target is in another workspace".to_string(),
                    ));
                }
                if target.mailbox_cursor.is_some() {
                    return Err(RuntimeError::InvalidArgument(
                        "handoff target has already consumed mailbox items".to_string(),
                    ));
                }
                if !matches!(
                    target.status,
                    AgentThreadStatus::Provisioned | AgentThreadStatus::Active
                ) {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "agent_thread",
                        from: target.status.as_db_str().to_string(),
                        to: "handoff target".to_string(),
                    });
                }
                let (run_id, run_generation, work_node_id, pending_children) =
                    match &request.run_id {
                        Some(run_id) => {
                            let run = lock_run(tx, &tenant_id, run_id).await?;
                            if run.agent_thread_id != source.id.to_string() {
                                return Err(RuntimeError::InvalidArgument(
                                    "handoff run is not owned by the source thread".to_string(),
                                ));
                            }
                            let state = crate::runtime::protocol_state::ProtocolStateStore::load(
                                tx,
                                &tenant_id,
                                &run_id.to_string(),
                            )
                            .await
                            .map_err(RuntimeError::from)?;
                            let children = state
                                .map(|state| state.child_agent_threads)
                                .unwrap_or_default();
                            (
                                Some(*run_id),
                                Some(run.generation),
                                Some(run.work_node_id),
                                children,
                            )
                        }
                        None => (
                            None,
                            None,
                            request.work_node_id,
                            Vec::new(),
                        ),
                    };
                let handoff_id = format!("ahf_{}", new_ulid());
                sqlx::query(
                    "INSERT INTO agent_handoffs (id, tenant_id, workspace_id, from_agent_thread_id, \
                     to_agent_thread_id, run_id, run_generation, work_node_id, mailbox_cursor, \
                     evidence_ids, pending_child_thread_ids, conversation_ref, status) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'PENDING')",
                )
                .bind(&handoff_id)
                .bind(&tenant_id)
                .bind(&source.workspace_id)
                .bind(source.id.to_string())
                .bind(target.id.to_string())
                .bind(run_id.as_ref().map(ToString::to_string))
                .bind(run_generation.map(|generation| generation.get() as i64))
                .bind(
                    work_node_id
                        .as_ref()
                        .map(ToString::to_string)
                        .or(request.work_node_id.as_ref().map(ToString::to_string)),
                )
                .bind(source.mailbox_cursor.as_deref())
                .bind(json!(request.evidence_ids))
                .bind(json!(pending_children))
                .bind(request.conversation_ref.as_deref())
                .execute(&mut **tx)
                .await?;
                let updated = sqlx::query(
                    "UPDATE agent_threads SET status = 'HANDING_OFF', \
                     handoff_to_agent_thread_id = $1, handoff_at = now() \
                     WHERE id = $2 AND tenant_id = $3 AND status = $4",
                )
                .bind(target.id.to_string())
                .bind(source.id.to_string())
                .bind(&tenant_id)
                .bind(source.status.as_db_str())
                .execute(&mut **tx)
                .await?;
                if updated.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: source.id.to_string(),
                    });
                }
                let source = load_thread_tx(tx, &tenant_id, &source.id).await?;
                let handoff = load_handoff_tx(tx, &tenant_id, &handoff_id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &source.id.to_string(),
                                                "agent.handoff_started",
                        &source.workspace_id,
                        source.generation,
                        json!({
                            "agent_thread_id": source.id.to_string(),
                            "from": AgentThreadStatus::Active.as_db_str(),
                            "to": source.status.as_db_str(),
                            "handoff_id": handoff.id,
                            "to_agent_thread_id": handoff.to_agent_thread_id.to_string(),
                            "run_id": handoff.run_id.as_ref().map(ToString::to_string),
                            "generation": source.generation.get(),
                        }),
                    )
                    .await?;
                Ok(handoff)
            })
        })
        .await
    }

    /// Complete a handoff, transferring the run's live work to the target.
    ///
    /// Completing an already-completed handoff returns the existing result and emits
    /// nothing, so it is idempotent.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the record is invisible,
    /// [`RuntimeError::HandoffNotRecoverable`] when it was rolled back, and
    /// [`RuntimeError::FencedStaleGeneration`] when the run moved on.
    pub async fn complete_handoff(
        &self,
        handoff_id: &str,
    ) -> Result<CompletedHandoff, RuntimeError> {
        let existing = self.load_handoff(handoff_id).await?;
        match existing.status {
            HandoffStatus::Completed => return self.completed_handoff(existing, true).await,
            HandoffStatus::RolledBack => {
                return Err(RuntimeError::HandoffNotRecoverable {
                    handoff_id: handoff_id.to_string(),
                    status: existing.status.as_db_str().to_string(),
                })
            }
            HandoffStatus::Pending => {}
        }
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let handoff_id = handoff_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let handoff = lock_handoff(tx, &tenant_id, &handoff_id).await?;
                if handoff.status != HandoffStatus::Pending {
                    return Err(RuntimeError::HandoffNotRecoverable {
                        handoff_id: handoff.id.clone(),
                        status: handoff.status.as_db_str().to_string(),
                    });
                }
                let source = lock_thread(tx, &tenant_id, &handoff.from_agent_thread_id).await?;
                if source.status != AgentThreadStatus::HandingOff {
                    return Err(RuntimeError::HandoffNotRecoverable {
                        handoff_id: handoff.id.clone(),
                        status: format!("source {}", source.status.as_db_str()),
                    });
                }
                let target = lock_thread(tx, &tenant_id, &handoff.to_agent_thread_id).await?;
                if target.status == AgentThreadStatus::Provisioned {
                    let updated = sqlx::query(
                        "UPDATE agent_threads SET status = 'ACTIVE' \
                         WHERE id = $1 AND tenant_id = $2 AND status = 'PROVISIONED'",
                    )
                    .bind(target.id.to_string())
                    .bind(&tenant_id)
                    .execute(&mut **tx)
                    .await?;
                    if updated.rows_affected() != 1 {
                        return Err(RuntimeError::StateConflict {
                            entity: "agent_thread",
                            id: target.id.to_string(),
                        });
                    }
                    publisher
                        .emit(
                            tx,
                            batch,
                            "agent_thread",
                            &target.id.to_string(),
                                                        "agent.activated",
                            &target.workspace_id,
                            target.generation,
                            json!({
                                "agent_thread_id": target.id.to_string(),
                                "from": AgentThreadStatus::Provisioned.as_db_str(),
                                "to": AgentThreadStatus::Active.as_db_str(),
                                "generation": target.generation.get(),
                            }),
                        )
                        .await?;
                } else if target.status != AgentThreadStatus::Active {
                    return Err(RuntimeError::IllegalTransition {
                        entity: "agent_thread",
                        from: target.status.as_db_str().to_string(),
                        to: "handoff target".to_string(),
                    });
                }
                let cursor = MailboxCursor::parse(handoff.mailbox_cursor.as_deref())?;
                let mut run_id = None;
                if let Some(run) = &handoff.run_id {
                    let current = lock_run(tx, &tenant_id, run).await?;
                    if let Some(recorded) = handoff.run_generation {
                        fence(current.generation, recorded)?;
                    }
                    if current.agent_thread_id != source.id.to_string() {
                        return Err(RuntimeError::InvalidArgument(
                            "handoff run is no longer owned by the source thread".to_string(),
                        ));
                    }
                    sqlx::query(
                        "UPDATE runs SET agent_thread_id = $1 WHERE id = $2 AND tenant_id = $3",
                    )
                    .bind(target.id.to_string())
                    .bind(run.to_string())
                    .bind(&tenant_id)
                    .execute(&mut **tx)
                    .await?;
                    run_id = Some(*run);
                }
                if let Some(node) = &handoff.work_node_id {
                    sqlx::query(
                        "UPDATE work_nodes SET owner_agent_thread_id = $1 \
                         WHERE id = $2 AND tenant_id = $3",
                    )
                    .bind(target.id.to_string())
                    .bind(node.to_string())
                    .bind(&tenant_id)
                    .execute(&mut **tx)
                    .await?;
                }
                if cursor > MailboxCursor::INITIAL {
                    sqlx::query(
                        "INSERT INTO agent_mailbox_items (agent_thread_id, seq, tenant_id, \
                         workspace_id, message_id, payload, delivered_at) \
                         SELECT $1, seq, tenant_id, workspace_id, message_id, payload, delivered_at \
                         FROM agent_mailbox_items WHERE tenant_id = $2 AND agent_thread_id = $3 \
                         AND seq > $4 ON CONFLICT DO NOTHING",
                    )
                    .bind(target.id.to_string())
                    .bind(&tenant_id)
                    .bind(source.id.to_string())
                    .bind(cursor.get() as i64)
                    .execute(&mut **tx)
                    .await?;
                }
                sqlx::query(
                    "UPDATE agent_threads SET mailbox_cursor = $1 WHERE id = $2 AND tenant_id = $3",
                )
                .bind(handoff.mailbox_cursor.as_deref())
                .bind(target.id.to_string())
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                let source_updated = sqlx::query(
                    "UPDATE agent_threads SET status = 'HANDED_OFF' \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'HANDING_OFF'",
                )
                .bind(source.id.to_string())
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                if source_updated.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: source.id.to_string(),
                    });
                }
                sqlx::query(
                    "UPDATE agent_handoffs SET status = 'COMPLETED', completed_at = now() \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'PENDING'",
                )
                .bind(&handoff.id)
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                let source = load_thread_tx(tx, &tenant_id, &source.id).await?;
                let target = load_thread_tx(tx, &tenant_id, &target.id).await?;
                let handoff = load_handoff_tx(tx, &tenant_id, &handoff.id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &source.id.to_string(),
                                                "agent.handoff_completed",
                        &source.workspace_id,
                        source.generation,
                        json!({
                            "agent_thread_id": source.id.to_string(),
                            "from": AgentThreadStatus::HandingOff.as_db_str(),
                            "to": source.status.as_db_str(),
                            "handoff_id": handoff.id,
                            "to_agent_thread_id": target.id.to_string(),
                            "run_id": run_id.as_ref().map(ToString::to_string),
                            "mailbox_cursor": handoff.mailbox_cursor,
                            "generation": source.generation.get(),
                        }),
                    )
                    .await?;
                Ok(CompletedHandoff {
                    handoff,
                    source,
                    target,
                    run_id,
                    duplicate: false,
                    // The target activation event, when any, is already staged above.
                })
            })
        })
        .await
    }

    /// Roll a `PENDING` handoff back: the source keeps its work.
    ///
    /// # Errors
    /// Returns [`RuntimeError::HandoffNotRecoverable`] when the record is not `PENDING`.
    pub async fn rollback_handoff(&self, handoff_id: &str) -> Result<AgentHandoff, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let handoff_id = handoff_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let handoff = lock_handoff(tx, &tenant_id, &handoff_id).await?;
                if handoff.status != HandoffStatus::Pending {
                    return Err(RuntimeError::HandoffNotRecoverable {
                        handoff_id: handoff.id.clone(),
                        status: handoff.status.as_db_str().to_string(),
                    });
                }
                let source = lock_thread(tx, &tenant_id, &handoff.from_agent_thread_id).await?;
                if source.status != AgentThreadStatus::HandingOff {
                    return Err(RuntimeError::HandoffNotRecoverable {
                        handoff_id: handoff.id.clone(),
                        status: format!("source {}", source.status.as_db_str()),
                    });
                }
                let restored = sqlx::query(
                    "UPDATE agent_threads SET status = 'ACTIVE', handoff_to_agent_thread_id = NULL, \
                     handoff_at = NULL WHERE id = $1 AND tenant_id = $2 AND status = 'HANDING_OFF'",
                )
                .bind(source.id.to_string())
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                if restored.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: source.id.to_string(),
                    });
                }
                sqlx::query(
                    "UPDATE agent_handoffs SET status = 'ROLLED_BACK' \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'PENDING'",
                )
                .bind(&handoff.id)
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                let source = load_thread_tx(tx, &tenant_id, &source.id).await?;
                let handoff = load_handoff_tx(tx, &tenant_id, &handoff.id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &source.id.to_string(),
                                                "agent.activated",
                        &source.workspace_id,
                        source.generation,
                        json!({
                            "agent_thread_id": source.id.to_string(),
                            "from": AgentThreadStatus::HandingOff.as_db_str(),
                            "to": source.status.as_db_str(),
                            "handoff_id": handoff.id,
                            "rolled_back": true,
                            "generation": source.generation.get(),
                        }),
                    )
                    .await?;
                Ok(handoff)
            })
        })
        .await
    }

    /// Pending handoffs in a workspace, for recovery to finish or roll back.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn pending_handoffs(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<AgentHandoff>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {HANDOFF_COLUMNS} FROM agent_handoffs \
             WHERE tenant_id = $1 AND workspace_id = $2 AND status = 'PENDING' \
             ORDER BY created_at, id"
        ))
        .bind(&tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let handoffs = rows
            .iter()
            .map(handoff_from_row)
            .collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(handoffs)
    }

    // ------------------------------------------------------------------ join

    /// Join a worker's outcome into its parent, or replay an existing merge.
    ///
    /// The `ACTIVE → JOINING → JOINED` chain and the once-only lineage merge commit in
    /// one transaction; DOMAIN.md §9.2 names `agent.joined` as the event. A teammate is
    /// refused, and joining the same child twice returns the existing merge unchanged.
    ///
    /// # Errors
    /// Returns [`RuntimeError::LifecyclePolicy`] for a teammate,
    /// [`RuntimeError::IllegalTransition`] unless the worker is `ACTIVE` or `JOINING`,
    /// [`RuntimeError::InvalidArgument`] when the parent does not match the edge,
    /// [`RuntimeError::NotFound`] for an invisible thread.
    pub async fn join_worker(&self, request: JoinRequest) -> Result<Joined, RuntimeError> {
        let child = self.load_thread(&request.child_agent_thread_id).await?;
        fence(child.generation, request.child_generation)?;
        if child.agent_kind == AgentKind::Teammate {
            return Err(RuntimeError::LifecyclePolicy {
                from: child.status.as_db_str().to_string(),
                to: AgentThreadStatus::Joined.as_db_str().to_string(),
                rule: TEAMMATE_CANNOT_JOIN,
            });
        }
        if child.status == AgentThreadStatus::Joined {
            return self.existing_join(child).await;
        }
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let mut publisher = Publisher::new(&tenant_id, &identity);
                let child = lock_thread(tx, &tenant_id, &request.child_agent_thread_id).await?;
                fence(child.generation, request.child_generation)?;
                if child.agent_kind == AgentKind::Teammate {
                    return Err(RuntimeError::LifecyclePolicy {
                        from: child.status.as_db_str().to_string(),
                        to: AgentThreadStatus::Joined.as_db_str().to_string(),
                        rule: TEAMMATE_CANNOT_JOIN,
                    });
                }
                if child.status == AgentThreadStatus::Joined {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: child.id.to_string(),
                    });
                }
                let parent_id = child.parent_id.ok_or_else(|| {
                    RuntimeError::InvalidArgument(
                        "a worker without a parent agent thread cannot join".to_string(),
                    )
                })?;
                let parent = lock_thread(tx, &tenant_id, &parent_id).await?;
                let edge = lock_edge(tx, &tenant_id, &parent.id, &child.id).await?;
                if edge.joined_at.is_some() {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: child.id.to_string(),
                    });
                }
                // Enter and leave JOINING inside this one transaction: DOMAIN.md §9.2
                // names agent.joined but no separate event for the intermediate.
                for (from, to) in [
                    (child.status, AgentThreadStatus::Joining),
                    (AgentThreadStatus::Joining, AgentThreadStatus::Joined),
                ] {
                    check_transition(child.agent_kind, from, to)?;
                }
                sqlx::query(
                    "UPDATE agent_threads SET status = 'JOINING' \
                     WHERE id = $1 AND tenant_id = $2 AND status = $3",
                )
                .bind(child.id.to_string())
                .bind(&tenant_id)
                .bind(child.status.as_db_str())
                .execute(&mut **tx)
                .await?;
                let joined = sqlx::query(
                    "UPDATE agent_threads SET status = 'JOINED' \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'JOINING'",
                )
                .bind(child.id.to_string())
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                if joined.rows_affected() != 1 {
                    return Err(RuntimeError::StateConflict {
                        entity: "agent_thread",
                        id: child.id.to_string(),
                    });
                }
                sqlx::query(
                    "UPDATE agent_graph_edges SET joined_at = now() \
                     WHERE id = $1 AND tenant_id = $2 AND joined_at IS NULL",
                )
                .bind(&edge.id)
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "INSERT INTO agent_joins (child_agent_thread_id, tenant_id, workspace_id, \
                     parent_agent_thread_id, edge_id, child_status, evidence_ids, artifact_ids, \
                     outcome) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                )
                .bind(child.id.to_string())
                .bind(&tenant_id)
                .bind(&child.workspace_id)
                .bind(parent.id.to_string())
                .bind(&edge.id)
                .bind(&request.child_status)
                .bind(json!(request.evidence_ids))
                .bind(json!(request.artifact_ids))
                .bind(&request.outcome)
                .execute(&mut **tx)
                .await?;
                let child = load_thread_tx(tx, &tenant_id, &child.id).await?;
                let edge = load_edge_tx(tx, &tenant_id, &edge.id).await?;
                let merge = load_join_tx(tx, &tenant_id, &child.id).await?;
                publisher
                    .emit(
                        tx,
                        batch,
                        "agent_thread",
                        &child.id.to_string(),
                        "agent.joined",
                        &child.workspace_id,
                        child.generation,
                        json!({
                            "agent_thread_id": child.id.to_string(),
                            "parent_agent_thread_id": parent.id.to_string(),
                            "from": AgentThreadStatus::Joining.as_db_str(),
                            "to": child.status.as_db_str(),
                            "status": merge.child_status,
                            "evidence_ids": merge.evidence_ids,
                            "artifact_ids": merge.artifact_ids,
                            "generation": child.generation.get(),
                        }),
                    )
                    .await?;
                let parent_run_resumed = release_parent_run(
                    tx,
                    batch,
                    &tenant_id,
                    &mut publisher,
                    &parent,
                    &child,
                    &request,
                )
                .await?;
                Ok(Joined {
                    child,
                    edge,
                    merge,
                    parent_run_resumed,
                    duplicate: false,
                })
            })
        })
        .await
    }

    async fn existing_join(&self, child: AgentThread) -> Result<Joined, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let merge = load_join_tx(&mut tx, &tenant_id, &child.id).await?;
        let edge = load_edge_tx(&mut tx, &tenant_id, &merge.edge_id).await?;
        tx.commit().await?;
        Ok(Joined {
            child,
            edge,
            merge,
            parent_run_resumed: false,
            duplicate: true,
        })
    }

    // ------------------------------------------------------------------ read paths

    /// Read one AgentThread.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the thread is not visible in this tenant.
    pub async fn load_thread(&self, id: &CanonicalId) -> Result<AgentThread, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let thread = load_thread_tx(&mut tx, &tenant_id, id).await?;
        tx.commit().await?;
        Ok(thread)
    }

    /// List a workspace's agent threads.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_threads(&self, workspace_id: &str) -> Result<Vec<AgentThread>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {THREAD_COLUMNS} FROM agent_threads \
             WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id"
        ))
        .bind(&tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let threads = rows.iter().map(thread_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(threads)
    }

    /// List a workspace's delegation edges.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_delegations(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<DelegationEdge>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows: Vec<PgRow> = sqlx::query(
            "SELECT e.id, e.tenant_id, e.parent_agent_thread_id, e.child_agent_thread_id, \
                    e.work_node_id, e.delegation_capability_id, e.delegated_at, e.joined_at \
             FROM agent_graph_edges e JOIN agent_threads c ON c.id = e.child_agent_thread_id \
             WHERE e.tenant_id = $1 AND c.workspace_id = $2 ORDER BY e.delegated_at, e.id",
        )
        .bind(&tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let edges = rows.iter().map(edge_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(edges)
    }

    /// Read one join merge.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when no merge exists for the child.
    pub async fn load_join(&self, child: &CanonicalId) -> Result<AgentJoin, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let merge = load_join_tx(&mut tx, &tenant_id, child).await?;
        tx.commit().await?;
        Ok(merge)
    }

    /// Read one handoff record.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the record is not visible in this tenant.
    pub async fn load_handoff(&self, handoff_id: &str) -> Result<AgentHandoff, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let handoff = load_handoff_tx(&mut tx, &tenant_id, handoff_id).await?;
        tx.commit().await?;
        Ok(handoff)
    }

    async fn completed_handoff(
        &self,
        handoff: AgentHandoff,
        duplicate: bool,
    ) -> Result<CompletedHandoff, RuntimeError> {
        let source = self.load_thread(&handoff.from_agent_thread_id).await?;
        let target = self.load_thread(&handoff.to_agent_thread_id).await?;
        Ok(CompletedHandoff {
            run_id: handoff.run_id,
            handoff,
            source,
            target,
            duplicate,
        })
    }

    async fn find_mailbox_item(
        &self,
        agent_thread_id: &CanonicalId,
        message_id: &str,
    ) -> Result<Option<MailboxItem>, RuntimeError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {MAILBOX_COLUMNS} FROM agent_mailbox_items \
             WHERE tenant_id = $1 AND agent_thread_id = $2 AND message_id = $3"
        ))
        .bind(&tenant_id)
        .bind(agent_thread_id.to_string())
        .bind(message_id)
        .fetch_optional(&mut *tx)
        .await?;
        let item = row.map(|row| mailbox_from_row(&row)).transpose()?;
        tx.commit().await?;
        Ok(item)
    }
}

// The run row a handoff or join needs, read directly so the agents store does not depend
// on the state machine's private loaders.
struct RunRef {
    id: String,
    status: String,
    generation: Generation,
    agent_thread_id: String,
    work_node_id: CanonicalId,
}

async fn lock_run(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    run_id: &CanonicalId,
) -> Result<RunRef, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(
        "SELECT id, status, generation, agent_thread_id, work_node_id FROM runs \
         WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
    )
    .bind(run_id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    let row = row.ok_or_else(|| RuntimeError::NotFound {
        entity: "run",
        id: run_id.to_string(),
        tenant_id: tenant_id.to_string(),
    })?;
    Ok(RunRef {
        id: row.try_get("id")?,
        status: row.try_get("status")?,
        generation: generation_from_i64(row.try_get("generation")?)?,
        agent_thread_id: row.try_get("agent_thread_id")?,
        work_node_id: parse_typed(&row.try_get::<String, _>("work_node_id")?, Prefix::WorkNode)?,
    })
}

/// Release a parent run once this worker and all of its siblings have joined.
///
/// `RuntimeStore::resolve_wait` resumes a `WAITING_CHILD` run on the first child result;
/// fanout needs the run to stay parked while any child is unfinished, so the remaining
/// list is checked here and the run only resumes when it is empty.
async fn release_parent_run(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    tenant_id: &str,
    publisher: &mut Publisher<'_>,
    parent: &AgentThread,
    child: &AgentThread,
    request: &JoinRequest,
) -> Result<bool, RuntimeError> {
    let Some(run_id) = &request.parent_run_id else {
        return Ok(false);
    };
    let run = lock_run(tx, tenant_id, run_id).await?;
    if let Some(generation) = request.parent_run_generation {
        fence(run.generation, generation)?;
    }
    if run.status != "WAITING_CHILD" || run.agent_thread_id != parent.id.to_string() {
        return Ok(false);
    }
    let mut state =
        crate::runtime::protocol_state::ProtocolStateStore::load(tx, tenant_id, &run.id)
            .await
            .map_err(RuntimeError::from)?
            .ok_or_else(|| RuntimeError::StateConflict {
                entity: "run",
                id: run.id.clone(),
            })?;
    if let Some(index) = state
        .child_agent_threads
        .iter()
        .position(|id| id == &child.id.to_string())
    {
        state.child_agent_threads.remove(index);
    }
    crate::runtime::protocol_state::ProtocolStateStore::store(tx, tenant_id, &state)
        .await
        .map_err(RuntimeError::from)?;
    if !state.child_agent_threads.is_empty() {
        // Siblings are still running: the run stays parked, and `agent.joined` already
        // gives this transaction its event.
        return Ok(false);
    }
    let released = sqlx::query(
        "UPDATE runs SET status = 'RUNNING' WHERE id = $1 AND tenant_id = $2 AND status = 'WAITING_CHILD'",
    )
    .bind(&run.id)
    .bind(tenant_id)
    .execute(&mut **tx)
    .await?;
    if released.rows_affected() != 1 {
        return Err(RuntimeError::StateConflict {
            entity: "run",
            id: run.id.clone(),
        });
    }
    publisher
        .emit(
            tx,
            batch,
            "run",
            &run.id,
            "run.resumed",
            &parent.workspace_id,
            run.generation,
            json!({
                "run_id": run.id,
                "from": "WAITING_CHILD",
                "to": "RUNNING",
                "generation": run.generation.get(),
            }),
        )
        .await?;
    Ok(true)
}

// ---------------------------------------------------------------- tx helpers

async fn insert_thread(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    thread: &NewAgentThread,
) -> Result<AgentThread, RuntimeError> {
    if let Some(definition) = &thread.definition_id {
        expect_prefix(definition, Prefix::Teammate)?;
    }
    if let Some(parent) = &thread.parent_id {
        expect_prefix(parent, Prefix::AgentThread)?;
    }
    if let Some(node) = &thread.work_node_id {
        expect_prefix(node, Prefix::WorkNode)?;
    }
    if let Some(projection) = &thread.capability_projection_id {
        expect_prefix(projection, Prefix::CapabilityProjection)?;
    }
    let id = CanonicalId::generate(Prefix::AgentThread, &mut UlidGenerator::new());
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, definition_id, \
         parent_id, work_node_id, capability_projection_id, generation, status, \
         execution_target_id, budget_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'PROVISIONED', $10, $11)",
    )
    .bind(id.to_string())
    .bind(tenant_id)
    .bind(&thread.workspace_id)
    .bind(thread.agent_kind.as_db_str())
    .bind(thread.definition_id.as_ref().map(ToString::to_string))
    .bind(thread.parent_id.as_ref().map(ToString::to_string))
    .bind(thread.work_node_id.as_ref().map(ToString::to_string))
    .bind(
        thread
            .capability_projection_id
            .as_ref()
            .map(ToString::to_string),
    )
    .bind(Generation::INITIAL.get() as i64)
    .bind(thread.execution_target_id.as_ref().map(ToString::to_string))
    .bind(&thread.budget_id)
    .execute(&mut **tx)
    .await?;
    load_thread_tx(tx, tenant_id, &id).await
}

async fn insert_mailbox_item(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    thread: &AgentThread,
    item: &NewMailboxItem,
) -> Result<MailboxItem, RuntimeError> {
    let seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_mailbox_items \
         WHERE agent_thread_id = $1 AND tenant_id = $2",
    )
    .bind(thread.id.to_string())
    .bind(tenant_id)
    .fetch_one(&mut **tx)
    .await?;
    let inserted = sqlx::query(
        "INSERT INTO agent_mailbox_items (agent_thread_id, seq, tenant_id, workspace_id, \
         message_id, payload) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (agent_thread_id, message_id) DO NOTHING",
    )
    .bind(thread.id.to_string())
    .bind(seq)
    .bind(tenant_id)
    .bind(&thread.workspace_id)
    .bind(&item.message_id)
    .bind(&item.payload)
    .execute(&mut **tx)
    .await?;
    if inserted.rows_affected() == 0 {
        return load_mailbox_item_tx(tx, tenant_id, &thread.id, &item.message_id).await;
    }
    load_mailbox_item_tx(tx, tenant_id, &thread.id, &item.message_id).await
}

async fn emit_mailbox_delivered(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    publisher: &mut Publisher<'_>,
    thread: &AgentThread,
    item: &MailboxItem,
) -> Result<(), RuntimeError> {
    publisher
        .emit(
            tx,
            batch,
            "agent_thread",
            &thread.id.to_string(),
            "agent.mailbox_delivered",
            &thread.workspace_id,
            thread.generation,
            json!({
                "agent_thread_id": thread.id.to_string(),
                "seq": item.seq,
                "message_id": item.message_id,
                "generation": thread.generation.get(),
            }),
        )
        .await
}

fn check_transition(
    kind: AgentKind,
    from: AgentThreadStatus,
    to: AgentThreadStatus,
) -> Result<(), RuntimeError> {
    if !from.can_transition_to(to) {
        return Err(RuntimeError::IllegalTransition {
            entity: "agent_thread",
            from: from.as_db_str().to_string(),
            to: to.as_db_str().to_string(),
        });
    }
    check_lifecycle_policy(kind, from, to)
}

async fn lock_thread(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<AgentThread, RuntimeError> {
    expect_prefix(id, Prefix::AgentThread)?;
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => thread_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "agent_thread",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_thread_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<AgentThread, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id.to_string())
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => thread_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "agent_thread",
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn lock_edge(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    parent: &CanonicalId,
    child: &CanonicalId,
) -> Result<DelegationEdge, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {EDGE_COLUMNS} FROM agent_graph_edges \
         WHERE tenant_id = $1 AND parent_agent_thread_id = $2 AND child_agent_thread_id = $3 \
         FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(parent.to_string())
    .bind(child.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => edge_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "delegation",
            id: child.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_edge_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    edge_id: &str,
) -> Result<DelegationEdge, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {EDGE_COLUMNS} FROM agent_graph_edges WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(edge_id)
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => edge_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "delegation",
            id: edge_id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_join_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    child: &CanonicalId,
) -> Result<AgentJoin, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {JOIN_COLUMNS} FROM agent_joins WHERE tenant_id = $1 AND child_agent_thread_id = $2"
    ))
    .bind(tenant_id)
    .bind(child.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => join_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "agent_join",
            id: child.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn lock_handoff(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    handoff_id: &str,
) -> Result<AgentHandoff, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {HANDOFF_COLUMNS} FROM agent_handoffs \
         WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(handoff_id)
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => handoff_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "agent_handoff",
            id: handoff_id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_handoff_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    handoff_id: &str,
) -> Result<AgentHandoff, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {HANDOFF_COLUMNS} FROM agent_handoffs WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(handoff_id)
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => handoff_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "agent_handoff",
            id: handoff_id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

async fn load_mailbox_item_tx(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    agent_thread_id: &CanonicalId,
    message_id: &str,
) -> Result<MailboxItem, RuntimeError> {
    let row: Option<PgRow> = sqlx::query(&format!(
        "SELECT {MAILBOX_COLUMNS} FROM agent_mailbox_items \
         WHERE tenant_id = $1 AND agent_thread_id = $2 AND message_id = $3"
    ))
    .bind(tenant_id)
    .bind(agent_thread_id.to_string())
    .bind(message_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => mailbox_from_row(&row),
        None => Err(RuntimeError::NotFound {
            entity: "mailbox_item",
            id: message_id.to_string(),
            tenant_id: tenant_id.to_string(),
        }),
    }
}

// ---------------------------------------------------------------- row decoding

fn generation_from_i64(value: i64) -> Result<Generation, RuntimeError> {
    let raw = u64::try_from(value)
        .map_err(|_| RuntimeError::InvalidArgument(format!("invalid generation {value}")))?;
    Generation::new(raw).map_err(|error| RuntimeError::InvalidArgument(error.to_string()))
}

fn expect_prefix(id: &CanonicalId, prefix: Prefix) -> Result<(), RuntimeError> {
    if id.prefix() != prefix {
        return Err(RuntimeError::InvalidArgument(format!(
            "expected a {} id, got {id}",
            prefix.as_str()
        )));
    }
    Ok(())
}

fn parse_typed(value: &str, prefix: Prefix) -> Result<CanonicalId, RuntimeError> {
    CanonicalId::parse_typed(value, prefix)
        .map_err(|_| RuntimeError::InvalidArgument(format!("invalid id {value:?}")))
}

fn optional_id(value: Option<String>, prefix: Prefix) -> Result<Option<CanonicalId>, RuntimeError> {
    value.map(|value| parse_typed(&value, prefix)).transpose()
}

fn new_ulid() -> String {
    UlidGenerator::new().generate().to_string()
}

fn thread_from_row(row: &PgRow) -> Result<AgentThread, RuntimeError> {
    Ok(AgentThread {
        id: parse_typed(&row.try_get::<String, _>("id")?, Prefix::AgentThread)?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        agent_kind: AgentKind::from_db_str(&row.try_get::<String, _>("agent_kind")?)?,
        definition_id: optional_id(row.try_get("definition_id")?, Prefix::Teammate)?,
        parent_id: optional_id(row.try_get("parent_id")?, Prefix::AgentThread)?,
        work_node_id: optional_id(row.try_get("work_node_id")?, Prefix::WorkNode)?,
        capability_projection_id: optional_id(
            row.try_get("capability_projection_id")?,
            Prefix::CapabilityProjection,
        )?,
        generation: generation_from_i64(row.try_get("generation")?)?,
        status: AgentThreadStatus::from_db_str(&row.try_get::<String, _>("status")?)?,
        mailbox_cursor: row.try_get("mailbox_cursor")?,
        execution_target_id: optional_id(
            row.try_get("execution_target_id")?,
            Prefix::ExecutionTarget,
        )?,
        budget_id: row.try_get("budget_id")?,
        suspended_reason: row.try_get("suspended_reason")?,
        handoff_to_agent_thread_id: optional_id(
            row.try_get("handoff_to_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        handoff_at: row.try_get("handoff_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn edge_from_row(row: &PgRow) -> Result<DelegationEdge, RuntimeError> {
    let id: String = row.try_get("id")?;
    if !id.starts_with("age_") {
        return Err(RuntimeError::InvalidArgument(format!(
            "invalid delegation id {id:?}"
        )));
    }
    Ok(DelegationEdge {
        id,
        tenant_id: row.try_get("tenant_id")?,
        parent_agent_thread_id: parse_typed(
            &row.try_get::<String, _>("parent_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        child_agent_thread_id: parse_typed(
            &row.try_get::<String, _>("child_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        work_node_id: optional_id(row.try_get("work_node_id")?, Prefix::WorkNode)?,
        delegation_capability_id: optional_id(
            row.try_get("delegation_capability_id")?,
            Prefix::CapabilityProjection,
        )?,
        delegated_at: row.try_get("delegated_at")?,
        joined_at: row.try_get("joined_at")?,
    })
}

fn mailbox_from_row(row: &PgRow) -> Result<MailboxItem, RuntimeError> {
    Ok(MailboxItem {
        agent_thread_id: parse_typed(
            &row.try_get::<String, _>("agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        seq: row.try_get("seq")?,
        message_id: row.try_get("message_id")?,
        payload: row.try_get("payload")?,
        delivered_at: row.try_get("delivered_at")?,
    })
}

fn handoff_from_row(row: &PgRow) -> Result<AgentHandoff, RuntimeError> {
    let id: String = row.try_get("id")?;
    if !id.starts_with("ahf_") {
        return Err(RuntimeError::InvalidArgument(format!(
            "invalid handoff id {id:?}"
        )));
    }
    Ok(AgentHandoff {
        id,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        from_agent_thread_id: parse_typed(
            &row.try_get::<String, _>("from_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        to_agent_thread_id: parse_typed(
            &row.try_get::<String, _>("to_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        run_id: optional_id(row.try_get("run_id")?, Prefix::Run)?,
        run_generation: row
            .try_get::<Option<i64>, _>("run_generation")?
            .map(generation_from_i64)
            .transpose()?,
        work_node_id: optional_id(row.try_get("work_node_id")?, Prefix::WorkNode)?,
        mailbox_cursor: row.try_get("mailbox_cursor")?,
        evidence_ids: row.try_get("evidence_ids")?,
        pending_child_thread_ids: row.try_get("pending_child_thread_ids")?,
        conversation_ref: row.try_get("conversation_ref")?,
        status: HandoffStatus::from_db_str(&row.try_get::<String, _>("status")?)?,
        completed_at: row.try_get("completed_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn join_from_row(row: &PgRow) -> Result<AgentJoin, RuntimeError> {
    Ok(AgentJoin {
        child_agent_thread_id: parse_typed(
            &row.try_get::<String, _>("child_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        parent_agent_thread_id: parse_typed(
            &row.try_get::<String, _>("parent_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        edge_id: row.try_get("edge_id")?,
        child_status: row.try_get("child_status")?,
        evidence_ids: row.try_get("evidence_ids")?,
        artifact_ids: row.try_get("artifact_ids")?,
        outcome: row.try_get("outcome")?,
        joined_at: row.try_get("joined_at")?,
    })
}

// ---------------------------------------------------------------- event versioning

/// Stamps every RuntimeEvent of one transaction with the same identity and version.
///
/// This is the same versioning contract as `state_machine::store::Publisher`, duplicated
/// here because that type is private to its module; both read the committed event count
/// for the aggregate inside the transaction, so versions are monotonic and gap-free.
struct Publisher<'a> {
    tenant_id: &'a str,
    actor: &'a Actor,
    correlation_id: quansio_core::CorrelationId,
    causation_id: Option<&'a quansio_core::CausationId>,
    command_id: Option<quansio_core::CommandId>,
    versions: std::collections::HashMap<(String, String), u64>,
}

impl<'a> Publisher<'a> {
    fn new(tenant_id: &'a str, identity: &'a RuntimeIdentity) -> Self {
        Self {
            tenant_id,
            actor: &identity.actor,
            correlation_id: identity.correlation_id,
            causation_id: identity.causation_id.as_ref(),
            command_id: identity.command_id,
            versions: std::collections::HashMap::new(),
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
