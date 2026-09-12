//! AgentGraph store: `agent_threads` and `agent_graph_edges` (DOMAIN.md §4.3, §5.1).
//!
//! A delegation records the parent's delegation capability id on the edge and requires the
//! parent to exist in the same tenant and workspace. The Capability Projection algebra is
//! owned by RUN-005, so this store does not implement a second capability engine: it calls
//! a [`DelegationNarrowingCheck`] and ships [`StructuralDelegationCheck`] as the structural
//! rule provable today. RUN-005 plugs its algebra in through
//! [`GraphStore::delegate_with`] without touching the store.

use quansio_core::{CanonicalId, Generation, Prefix};
use sqlx::postgres::PgRow;
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::{Postgres, Row, Transaction};

use crate::error::{Entity, GraphError};
use crate::state::{AgentKind, AgentThreadStatus};
use crate::store::GraphStore;

/// Fields required to create an [`AgentThread`] (DOMAIN.md §5.1).
#[derive(Debug, Clone)]
pub struct NewAgentThread {
    /// Workspace the agent thread participates in.
    pub workspace_id: String,
    /// Persistent teammate or ephemeral worker.
    pub agent_kind: AgentKind,
    /// Teammate definition, for a persistent teammate.
    pub definition_id: Option<CanonicalId>,
    /// Delegating parent agent thread, when spawned by delegation.
    pub parent_id: Option<CanonicalId>,
    /// WorkNode this agent thread participates in.
    pub work_node_id: Option<CanonicalId>,
    /// Capability projection resolved before the thread becomes active.
    pub capability_projection_id: Option<CanonicalId>,
    /// Execution target bound to the thread.
    pub execution_target_id: Option<CanonicalId>,
    /// Budget row reference.
    pub budget_id: Option<String>,
}

impl NewAgentThread {
    /// A thread with DOMAIN defaults: no lineage, no projection, no target.
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
}

/// A persisted `agent_threads` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentThread {
    /// Canonical `ath_` identity.
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
    /// Mailbox cursor.
    pub mailbox_cursor: Option<String>,
    /// Execution target.
    pub execution_target_id: Option<CanonicalId>,
    /// Budget row reference.
    pub budget_id: Option<String>,
    /// Why the thread is suspended.
    pub suspended_reason: Option<String>,
    /// Handoff target.
    pub handoff_to_agent_thread_id: Option<CanonicalId>,
    /// Handoff time.
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
    /// The parent's delegation capability id recorded on the edge.
    pub delegation_capability_id: Option<CanonicalId>,
    /// When the delegation was recorded.
    pub delegated_at: DateTime<Utc>,
    /// When the worker joined its parent.
    pub joined_at: Option<DateTime<Utc>>,
}

/// A delegation: the created child thread and its AgentGraph edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Delegated {
    /// The child agent thread (created `PROVISIONED`).
    pub child: AgentThread,
    /// The delegation edge.
    pub edge: DelegationEdge,
}

/// Request to delegate work from a parent agent thread to a new child.
#[derive(Debug, Clone)]
pub struct DelegationRequest {
    /// The child thread to create.
    pub child: NewAgentThread,
    /// WorkNode the delegation serves; defaults to the child's `work_node_id`.
    pub work_node_id: Option<CanonicalId>,
    /// Explicit delegation capability id; defaults to the parent's projection.
    pub delegation_capability_id: Option<CanonicalId>,
}

impl DelegationRequest {
    /// A request that inherits the parent's capability projection and the child's node.
    #[must_use]
    pub fn new(child: NewAgentThread) -> Self {
        Self {
            work_node_id: child.work_node_id,
            child,
            delegation_capability_id: None,
        }
    }
}

/// Narrowing check applied to every delegation.
///
/// RUN-005 implements this trait with the Capability Projection algebra (DOMAIN.md §6.3);
/// until then [`StructuralDelegationCheck`] enforces the rule that is provable from the
/// graph alone. The store never inspects capability contents itself.
pub trait DelegationNarrowingCheck {
    /// Reject a delegation whose child does not record a narrowing of the parent.
    ///
    /// # Errors
    /// Returns [`GraphError::NarrowingRejected`] when the delegation is not a narrowing.
    fn check(
        &self,
        parent: &AgentThread,
        child: &NewAgentThread,
        delegation_capability_id: Option<&CanonicalId>,
    ) -> Result<(), GraphError>;
}

/// The structural narrowing rule shipped before RUN-005: the child records exactly the
/// parent's delegation capability id, and no capability is invented when the parent has no
/// projection yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuralDelegationCheck;

impl DelegationNarrowingCheck for StructuralDelegationCheck {
    fn check(
        &self,
        parent: &AgentThread,
        _child: &NewAgentThread,
        delegation_capability_id: Option<&CanonicalId>,
    ) -> Result<(), GraphError> {
        match (&parent.capability_projection_id, delegation_capability_id) {
            (Some(parent_capability), Some(recorded)) if recorded == parent_capability => Ok(()),
            (Some(_), _) => Err(GraphError::NarrowingRejected(
                "child must record the parent's delegation capability id".to_string(),
            )),
            (None, None) => Ok(()),
            (None, Some(_)) => Err(GraphError::NarrowingRejected(
                "parent holds no delegation capability, so the child cannot record one".to_string(),
            )),
        }
    }
}

const THREAD_COLUMNS: &str = "id, tenant_id, workspace_id, agent_kind, definition_id, parent_id, \
     work_node_id, capability_projection_id, generation, status, mailbox_cursor, \
     execution_target_id, budget_id, suspended_reason, handoff_to_agent_thread_id, handoff_at, \
     created_at, updated_at";

const EDGE_COLUMNS: &str = "id, tenant_id, parent_agent_thread_id, child_agent_thread_id, \
     work_node_id, delegation_capability_id, delegated_at, joined_at";

fn generation_from_i64(value: i64) -> Result<Generation, GraphError> {
    Generation::new(value as u64).map_err(|_| {
        GraphError::InvalidArgument(format!("invalid agent thread generation {value}"))
    })
}

fn thread_from_row(row: &PgRow) -> Result<AgentThread, GraphError> {
    Ok(AgentThread {
        id: CanonicalId::parse_typed(&row.try_get::<String, _>("id")?, Prefix::AgentThread)
            .map_err(|_| GraphError::InvalidId {
                value: row.try_get("id").unwrap_or_default(),
                expected: Prefix::AgentThread.as_str(),
            })?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        agent_kind: AgentKind::from_db_str(row.try_get("agent_kind")?)?,
        definition_id: GraphStore::optional_id(row.try_get("definition_id")?, Prefix::Teammate)?,
        parent_id: GraphStore::optional_id(row.try_get("parent_id")?, Prefix::AgentThread)?,
        work_node_id: GraphStore::optional_id(row.try_get("work_node_id")?, Prefix::WorkNode)?,
        capability_projection_id: GraphStore::optional_id(
            row.try_get("capability_projection_id")?,
            Prefix::CapabilityProjection,
        )?,
        generation: generation_from_i64(row.try_get("generation")?)?,
        status: AgentThreadStatus::from_db_str(row.try_get("status")?)?,
        mailbox_cursor: row.try_get("mailbox_cursor")?,
        execution_target_id: GraphStore::optional_id(
            row.try_get("execution_target_id")?,
            Prefix::ExecutionTarget,
        )?,
        budget_id: row.try_get("budget_id")?,
        suspended_reason: row.try_get("suspended_reason")?,
        handoff_to_agent_thread_id: GraphStore::optional_id(
            row.try_get("handoff_to_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        handoff_at: row.try_get("handoff_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn delegation_from_row(row: &PgRow) -> Result<DelegationEdge, GraphError> {
    let id: String = row.try_get("id")?;
    if !id.starts_with("age_") {
        return Err(GraphError::InvalidId {
            value: id,
            expected: "age_",
        });
    }
    Ok(DelegationEdge {
        id,
        tenant_id: row.try_get("tenant_id")?,
        parent_agent_thread_id: CanonicalId::parse_typed(
            &row.try_get::<String, _>("parent_agent_thread_id")?,
            Prefix::AgentThread,
        )
        .map_err(|_| GraphError::InvalidArgument("bad parent agent thread id".to_string()))?,
        child_agent_thread_id: CanonicalId::parse_typed(
            &row.try_get::<String, _>("child_agent_thread_id")?,
            Prefix::AgentThread,
        )
        .map_err(|_| GraphError::InvalidArgument("bad child agent thread id".to_string()))?,
        work_node_id: GraphStore::optional_id(row.try_get("work_node_id")?, Prefix::WorkNode)?,
        delegation_capability_id: GraphStore::optional_id(
            row.try_get("delegation_capability_id")?,
            Prefix::CapabilityProjection,
        )?,
        delegated_at: row.try_get("delegated_at")?,
        joined_at: row.try_get("joined_at")?,
    })
}

impl GraphStore {
    /// Create an `AgentThread` in `PROVISIONED` state.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the workspace, parent or WorkNode is not
    /// visible in this tenant.
    pub async fn create_agent_thread(
        &self,
        thread: NewAgentThread,
    ) -> Result<AgentThread, GraphError> {
        let mut tx = self.begin().await?;
        let created = self.create_agent_thread_tx(&mut tx, thread).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &created.workspace_id).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Read one agent thread by id.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the thread is not visible in this tenant.
    pub async fn get_agent_thread(&self, id: &CanonicalId) -> Result<AgentThread, GraphError> {
        let mut tx = self.begin().await?;
        let thread = self.get_agent_thread_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(thread)
    }

    /// Apply a legal AgentThread transition (DOMAIN.md §5.1).
    ///
    /// A persistent teammate never reaches `JOINED`; workers must.
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] for an illegal transition and
    /// [`GraphError::StateConflict`] when the row changed concurrently.
    pub async fn transition_agent_thread(
        &self,
        id: &CanonicalId,
        to: AgentThreadStatus,
    ) -> Result<AgentThread, GraphError> {
        let mut tx = self.begin().await?;
        let updated = self.transition_agent_thread_tx(&mut tx, id, to).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &updated.workspace_id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Delegate to a new child agent thread using the structural narrowing rule.
    ///
    /// # Errors
    /// See [`GraphStore::delegate_with`].
    pub async fn delegate(
        &self,
        parent_id: &CanonicalId,
        request: DelegationRequest,
    ) -> Result<Delegated, GraphError> {
        self.delegate_with(&StructuralDelegationCheck, parent_id, request)
            .await
    }

    /// Delegate with an explicit [`DelegationNarrowingCheck`].
    ///
    /// This is the RUN-005 seam: the real Capability Projection algebra is supplied here and
    /// runs inside the same transaction as the child thread and edge insert, so a rejected
    /// delegation writes nothing.
    ///
    /// # Errors
    /// Returns [`GraphError::ParentNotFound`] when the parent is not visible,
    /// [`GraphError::WorkspaceMismatch`] when the child belongs to another workspace, and
    /// [`GraphError::NarrowingRejected`] when the check rejects the delegation.
    pub async fn delegate_with<C: DelegationNarrowingCheck>(
        &self,
        check: &C,
        parent_id: &CanonicalId,
        request: DelegationRequest,
    ) -> Result<Delegated, GraphError> {
        let mut tx = self.begin().await?;
        Self::expect_prefix(parent_id, Prefix::AgentThread)?;
        let parent = self
            .lock_agent_thread_tx(&mut tx, parent_id)
            .await
            .map_err(|error| match error {
                GraphError::NotFound { .. } => GraphError::ParentNotFound {
                    parent_id: parent_id.to_string(),
                    tenant_id: self.tenant_id.clone(),
                    workspace_id: request.child.workspace_id.clone(),
                },
                other => other,
            })?;
        if parent.workspace_id != request.child.workspace_id {
            return Err(GraphError::WorkspaceMismatch {
                parent_workspace: parent.workspace_id.clone(),
                child_workspace: request.child.workspace_id.clone(),
            });
        }
        let delegated = self
            .delegate_inner_tx(&mut tx, check, &parent, &request)
            .await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &parent.workspace_id).await?;
        tx.commit().await?;
        Ok(delegated)
    }

    /// List a workspace's agent threads.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_agent_threads(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<AgentThread>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE tenant_id = $1 AND workspace_id = $2 \
             ORDER BY created_at, id"
        ))
        .bind(&self.tenant_id)
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
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_delegations(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<DelegationEdge>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(
            "SELECT e.id, e.tenant_id, e.parent_agent_thread_id, e.child_agent_thread_id, \
                    e.work_node_id, e.delegation_capability_id, e.delegated_at, e.joined_at \
             FROM agent_graph_edges e JOIN agent_threads c ON c.id = e.child_agent_thread_id \
             WHERE e.tenant_id = $1 AND c.workspace_id = $2 ORDER BY e.delegated_at, e.id",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let edges = rows
            .iter()
            .map(delegation_from_row)
            .collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(edges)
    }

    // ---------------------------------------------------------------- tx helpers

    pub(crate) async fn create_agent_thread_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        thread: NewAgentThread,
    ) -> Result<AgentThread, GraphError> {
        Self::ensure_workspace_tx(tx, &self.tenant_id, &thread.workspace_id).await?;
        if let Some(definition) = &thread.definition_id {
            Self::expect_prefix(definition, Prefix::Teammate)?;
        }
        if let Some(parent) = &thread.parent_id {
            Self::expect_prefix(parent, Prefix::AgentThread)?;
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT id FROM agent_threads WHERE id = $1 AND tenant_id = $2 AND workspace_id = $3",
            )
            .bind(parent.to_string())
            .bind(&self.tenant_id)
            .bind(&thread.workspace_id)
            .fetch_optional(&mut **tx)
            .await?;
            if exists.is_none() {
                return Err(GraphError::NotFound {
                    entity: Entity::AgentThread.as_str(),
                    id: parent.to_string(),
                    tenant_id: self.tenant_id.clone(),
                });
            }
        }
        if let Some(node) = &thread.work_node_id {
            Self::expect_prefix(node, Prefix::WorkNode)?;
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT id FROM work_nodes WHERE id = $1 AND tenant_id = $2 AND workspace_id = $3",
            )
            .bind(node.to_string())
            .bind(&self.tenant_id)
            .bind(&thread.workspace_id)
            .fetch_optional(&mut **tx)
            .await?;
            if exists.is_none() {
                return Err(GraphError::NotFound {
                    entity: Entity::WorkNode.as_str(),
                    id: node.to_string(),
                    tenant_id: self.tenant_id.clone(),
                });
            }
        }
        let id = self.generate_id(Prefix::AgentThread);
        sqlx::query(
            "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, definition_id, \
             parent_id, work_node_id, capability_projection_id, generation, status, \
             execution_target_id, budget_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'PROVISIONED', $10, $11)",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
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
        self.get_agent_thread_tx(tx, &id).await
    }

    pub(crate) async fn get_agent_thread_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<AgentThread, GraphError> {
        Self::expect_prefix(id, Prefix::AgentThread)?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE id = $1 AND tenant_id = $2"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => thread_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::AgentThread.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn transition_agent_thread_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        to: AgentThreadStatus,
    ) -> Result<AgentThread, GraphError> {
        let current = self.lock_agent_thread_tx(tx, id).await?;
        if current.agent_kind == AgentKind::Teammate && to == AgentThreadStatus::Joined {
            return Err(GraphError::IllegalTransition {
                entity: Entity::AgentThread,
                from: current.status.as_db_str().to_string(),
                to: to.as_db_str().to_string(),
            });
        }
        if !current.status.can_transition_to(to) {
            return Err(GraphError::IllegalTransition {
                entity: Entity::AgentThread,
                from: current.status.as_db_str().to_string(),
                to: to.as_db_str().to_string(),
            });
        }
        let updated = sqlx::query(
            "UPDATE agent_threads SET status = $1 WHERE id = $2 AND tenant_id = $3 AND status = $4",
        )
        .bind(to.as_db_str())
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(current.status.as_db_str())
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(GraphError::StateConflict {
                entity: Entity::AgentThread.as_str(),
                id: id.to_string(),
                expected: current.status.as_db_str().to_string(),
            });
        }
        self.get_agent_thread_tx(tx, id).await
    }

    pub(crate) async fn lock_agent_thread_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<AgentThread, GraphError> {
        Self::expect_prefix(id, Prefix::AgentThread)?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => thread_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::AgentThread.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn get_delegation_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &str,
    ) -> Result<DelegationEdge, GraphError> {
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {EDGE_COLUMNS} FROM agent_graph_edges WHERE id = $1 AND tenant_id = $2"
        ))
        .bind(id)
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => delegation_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::Delegation.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn delegate_inner_tx<C: DelegationNarrowingCheck>(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        check: &C,
        parent: &AgentThread,
        request: &DelegationRequest,
    ) -> Result<Delegated, GraphError> {
        if parent.workspace_id != request.child.workspace_id {
            return Err(GraphError::WorkspaceMismatch {
                parent_workspace: parent.workspace_id.clone(),
                child_workspace: request.child.workspace_id.clone(),
            });
        }
        let resolved = request
            .delegation_capability_id
            .or(parent.capability_projection_id);
        if let Some(capability) = &resolved {
            Self::expect_prefix(capability, Prefix::CapabilityProjection)?;
        }
        check.check(parent, &request.child, resolved.as_ref())?;
        let mut child = request.child.clone();
        child.parent_id = Some(parent.id);
        let created = self.create_agent_thread_tx(tx, child).await?;
        let edge_id = format!("age_{}", self.generate_ulid());
        let work_node_id = request.work_node_id.or(created.work_node_id);
        sqlx::query(
            "INSERT INTO agent_graph_edges (id, tenant_id, parent_agent_thread_id, \
             child_agent_thread_id, work_node_id, delegation_capability_id) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&edge_id)
        .bind(&self.tenant_id)
        .bind(parent.id.to_string())
        .bind(created.id.to_string())
        .bind(work_node_id.as_ref().map(ToString::to_string))
        .bind(resolved.as_ref().map(ToString::to_string))
        .execute(&mut **tx)
        .await?;
        let edge = self.get_delegation_tx(tx, &edge_id).await?;
        Ok(Delegated {
            child: created,
            edge,
        })
    }
}
