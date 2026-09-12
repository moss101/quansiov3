//! WorkGraph store: `work_nodes` and `work_edges` (DOMAIN.md §4.1–§4.2).
//!
//! Invariants enforced here, in code and by database constraints:
//!
//! * `depends_on`/`parent_of` edges may not close a cycle; the check runs inside the same
//!   transaction as the insert, so concurrent inserts cannot create one between the check
//!   and the write.
//! * Only the [`WorkNodeStatus`] transition table is legal.
//! * A node reaches `done` only through [`GraphStore::mark_verification_passed`]; the
//!   generic transition refuses a `done` target.
//! * Every mutation is compare-and-set on `quansio_core::Revision`; a stale revision
//!   returns [`GraphError::RevisionConflict`] and commits nothing.

use quansio_core::{CanonicalId, Prefix, Revision};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::{Postgres, Row, Transaction};

use crate::error::{Entity, GraphError};
use crate::state::{WorkEdgeKind, WorkNodeKind, WorkNodeStatus, WorkOrigin};
use crate::store::GraphStore;

/// Fields required to create a [`WorkNode`] (DOMAIN.md §4.1).
#[derive(Debug, Clone)]
pub struct NewWorkNode {
    /// Workspace the node belongs to.
    pub workspace_id: String,
    /// Node kind.
    pub kind: WorkNodeKind,
    /// Human-readable title.
    pub title: String,
    /// Optional description.
    pub description: Option<String>,
    /// Optional parent node.
    pub parent_id: Option<CanonicalId>,
    /// Agent thread currently owning the node, when assigned.
    pub owner_agent_thread_id: Option<CanonicalId>,
    /// Actor that created the node (`{kind, id}`).
    pub created_by: Value,
    /// CompletionContract (§4.4); `{}` when the node inherits one.
    pub completion_contract: Value,
    /// Capability need templates (§6).
    pub capability_needs: Value,
    /// Budget row reference (§13.2).
    pub budget_id: Option<String>,
    /// Priority 0–3.
    pub priority: i16,
    /// Thread the node originated from.
    pub thread_id: Option<CanonicalId>,
    /// How the node came into existence.
    pub origin: WorkOrigin,
}

impl NewWorkNode {
    /// A node with DOMAIN defaults: no parent, empty CompletionContract, priority 1,
    /// user origin.
    #[must_use]
    pub fn new(
        workspace_id: impl Into<String>,
        kind: WorkNodeKind,
        title: impl Into<String>,
        created_by: Value,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            kind,
            title: title.into(),
            description: None,
            parent_id: None,
            owner_agent_thread_id: None,
            created_by,
            completion_contract: json!({}),
            capability_needs: json!([]),
            budget_id: None,
            priority: 1,
            thread_id: None,
            origin: WorkOrigin::User,
        }
    }
}

/// A persisted `work_nodes` row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkNode {
    /// Canonical `wn_` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Node kind.
    pub kind: WorkNodeKind,
    /// Title.
    pub title: String,
    /// Description.
    pub description: Option<String>,
    /// Parent node.
    pub parent_id: Option<CanonicalId>,
    /// Current status.
    pub status: WorkNodeStatus,
    /// Owning agent thread.
    pub owner_agent_thread_id: Option<CanonicalId>,
    /// Creating actor.
    pub created_by: Value,
    /// CompletionContract.
    pub completion_contract: Value,
    /// Capability need templates.
    pub capability_needs: Value,
    /// Budget row reference.
    pub budget_id: Option<String>,
    /// Priority 0–3.
    pub priority: i16,
    /// Aggregate revision.
    pub revision: Revision,
    /// Originating thread.
    pub thread_id: Option<CanonicalId>,
    /// How the node came into existence.
    pub origin: WorkOrigin,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// Fields required to create a [`WorkEdge`] (DOMAIN.md §4.2).
#[derive(Debug, Clone)]
pub struct NewWorkEdge {
    /// Workspace both endpoints belong to.
    pub workspace_id: String,
    /// Edge source node.
    pub from_node_id: CanonicalId,
    /// Edge target node.
    pub to_node_id: CanonicalId,
    /// Edge kind.
    pub kind: WorkEdgeKind,
}

impl NewWorkEdge {
    /// An edge with the canonical fields.
    #[must_use]
    pub fn new(
        workspace_id: impl Into<String>,
        from_node_id: CanonicalId,
        to_node_id: CanonicalId,
        kind: WorkEdgeKind,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            from_node_id,
            to_node_id,
            kind,
        }
    }
}

/// A persisted `work_edges` row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkEdge {
    /// Canonical `we_` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Source node.
    pub from_node_id: CanonicalId,
    /// Target node.
    pub to_node_id: CanonicalId,
    /// Edge kind.
    pub kind: WorkEdgeKind,
    /// Aggregate revision.
    pub revision: Revision,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

fn node_from_row(row: &PgRow) -> Result<WorkNode, GraphError> {
    Ok(WorkNode {
        id: parse_id(row.try_get("id")?, Prefix::WorkNode)?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        kind: WorkNodeKind::from_db_str(row.try_get("kind")?)?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        parent_id: GraphStore::optional_id(row.try_get("parent_id")?, Prefix::WorkNode)?,
        status: WorkNodeStatus::from_db_str(row.try_get("status")?)?,
        owner_agent_thread_id: GraphStore::optional_id(
            row.try_get("owner_agent_thread_id")?,
            Prefix::AgentThread,
        )?,
        created_by: row.try_get("created_by")?,
        completion_contract: row.try_get("completion_contract")?,
        capability_needs: row.try_get("capability_needs")?,
        budget_id: row.try_get("budget_id")?,
        priority: row.try_get("priority")?,
        revision: Revision::new(row.try_get::<i64, _>("revision")? as u64),
        thread_id: GraphStore::optional_id(row.try_get("thread_id")?, Prefix::Thread)?,
        origin: WorkOrigin::from_db_str(row.try_get("origin")?)?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn edge_from_row(row: &PgRow) -> Result<WorkEdge, GraphError> {
    Ok(WorkEdge {
        id: parse_id(row.try_get("id")?, Prefix::WorkEdge)?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        from_node_id: parse_id(row.try_get("from_node_id")?, Prefix::WorkNode)?,
        to_node_id: parse_id(row.try_get("to_node_id")?, Prefix::WorkNode)?,
        kind: WorkEdgeKind::from_db_str(row.try_get("kind")?)?,
        revision: Revision::new(row.try_get::<i64, _>("revision")? as u64),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn parse_id(value: String, prefix: Prefix) -> Result<CanonicalId, GraphError> {
    CanonicalId::parse_typed(&value, prefix).map_err(|_| GraphError::InvalidId {
        value,
        expected: prefix.as_str(),
    })
}

const NODE_COLUMNS: &str = "id, tenant_id, workspace_id, kind, title, description, parent_id, status, \
     owner_agent_thread_id, created_by, completion_contract, capability_needs, budget_id, priority, \
     revision, thread_id, origin, created_at, updated_at";

const EDGE_COLUMNS: &str =
    "id, tenant_id, workspace_id, from_node_id, to_node_id, kind, revision, created_at, updated_at";

impl GraphStore {
    /// Create a node and advance the workspace graph revision.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the workspace or parent node is not visible in
    /// this tenant, and [`GraphError::InvalidId`] for a non-`wn_` parent id.
    pub async fn create_node(&self, node: NewWorkNode) -> Result<WorkNode, GraphError> {
        let mut tx = self.begin().await?;
        let created = self.create_node_tx(&mut tx, node).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &created.workspace_id).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Read one node by id.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the node is not visible in this tenant.
    pub async fn get_node(&self, id: &CanonicalId) -> Result<WorkNode, GraphError> {
        let mut tx = self.begin().await?;
        let node = self.get_node_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(node)
    }

    /// List a workspace's nodes, ordered by creation.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_nodes(&self, workspace_id: &str) -> Result<Vec<WorkNode>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {NODE_COLUMNS} FROM work_nodes WHERE tenant_id = $1 AND workspace_id = $2 \
             ORDER BY created_at, id"
        ))
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let nodes = rows.iter().map(node_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(nodes)
    }

    /// Create an edge, rejecting cycles for `depends_on`/`parent_of`.
    ///
    /// # Errors
    /// Returns [`GraphError::Cycle`] for a cycle, and [`GraphError::NotFound`] when either
    /// endpoint is not visible in this tenant/workspace.
    pub async fn create_edge(&self, edge: NewWorkEdge) -> Result<WorkEdge, GraphError> {
        let mut tx = self.begin().await?;
        let created = self.create_edge_tx(&mut tx, edge).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &created.workspace_id).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Read one edge by id.
    ///
    /// # Errors
    /// Returns [`GraphError::NotFound`] when the edge is not visible in this tenant.
    pub async fn get_edge(&self, id: &CanonicalId) -> Result<WorkEdge, GraphError> {
        let mut tx = self.begin().await?;
        let edge = self.get_edge_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(edge)
    }

    /// List a workspace's edges.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the query fails.
    pub async fn list_edges(&self, workspace_id: &str) -> Result<Vec<WorkEdge>, GraphError> {
        let mut tx = self.begin().await?;
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT {EDGE_COLUMNS} FROM work_edges WHERE tenant_id = $1 AND workspace_id = $2 \
             ORDER BY created_at, id"
        ))
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let edges = rows.iter().map(edge_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(edges)
    }

    /// Remove an edge, compare-and-set on its revision.
    ///
    /// # Errors
    /// Returns [`GraphError::RevisionConflict`] for a stale revision and
    /// [`GraphError::NotFound`] when the edge is not visible in this tenant.
    pub async fn remove_edge(
        &self,
        id: &CanonicalId,
        expected: Revision,
    ) -> Result<(), GraphError> {
        let mut tx = self.begin().await?;
        let removed = self.remove_edge_tx(&mut tx, id, expected).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &removed.workspace_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Apply a legal WorkNode transition, compare-and-set on `revision`.
    ///
    /// `done` is never accepted here: it is reachable only through
    /// [`GraphStore::mark_verification_passed`] (DOMAIN.md §4.4).
    ///
    /// # Errors
    /// Returns [`GraphError::VerificationRequired`] for a `done` target,
    /// [`GraphError::IllegalTransition`] for an illegal edge in the state machine and
    /// [`GraphError::RevisionConflict`] for a stale revision. Nothing is written on error.
    pub async fn transition_node(
        &self,
        id: &CanonicalId,
        expected: Revision,
        to: WorkNodeStatus,
    ) -> Result<WorkNode, GraphError> {
        let mut tx = self.begin().await?;
        let updated = self.transition_node_tx(&mut tx, id, expected, to).await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &updated.workspace_id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Mark a node done after its CompletionContract verification passed.
    ///
    /// Requires the persisted status to be `verifying`, so a caller cannot shortcut the
    /// verification path.
    ///
    /// # Errors
    /// Returns [`GraphError::IllegalTransition`] when the node is not `verifying`, and
    /// [`GraphError::RevisionConflict`] for a stale revision. Nothing is written on error.
    pub async fn mark_verification_passed(
        &self,
        id: &CanonicalId,
        expected: Revision,
    ) -> Result<WorkNode, GraphError> {
        let mut tx = self.begin().await?;
        let updated = self
            .mark_verification_passed_tx(&mut tx, id, expected)
            .await?;
        Self::bump_head_tx(&mut tx, &self.tenant_id, &updated.workspace_id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    // ---------------------------------------------------------------- tx helpers

    pub(crate) async fn create_node_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        node: NewWorkNode,
    ) -> Result<WorkNode, GraphError> {
        Self::ensure_workspace_tx(tx, &self.tenant_id, &node.workspace_id).await?;
        let id = self.generate_id(Prefix::WorkNode);
        if node.priority < 0 || node.priority > 3 {
            return Err(GraphError::InvalidArgument(format!(
                "work node priority {} is outside 0..=3",
                node.priority
            )));
        }
        if let Some(parent) = &node.parent_id {
            Self::expect_prefix(parent, Prefix::WorkNode)?;
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT id FROM work_nodes WHERE id = $1 AND tenant_id = $2 AND workspace_id = $3",
            )
            .bind(parent.to_string())
            .bind(&self.tenant_id)
            .bind(&node.workspace_id)
            .fetch_optional(&mut **tx)
            .await?;
            if exists.is_none() {
                return Err(GraphError::NotFound {
                    entity: Entity::WorkNode.as_str(),
                    id: parent.to_string(),
                    tenant_id: self.tenant_id.clone(),
                });
            }
        }
        sqlx::query(
            "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, description, \
             parent_id, status, owner_agent_thread_id, created_by, completion_contract, \
             capability_needs, budget_id, priority, revision, thread_id, origin) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'draft', $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(&node.workspace_id)
        .bind(node.kind.as_db_str())
        .bind(&node.title)
        .bind(&node.description)
        .bind(node.parent_id.as_ref().map(ToString::to_string))
        .bind(node.owner_agent_thread_id.as_ref().map(ToString::to_string))
        .bind(&node.created_by)
        .bind(&node.completion_contract)
        .bind(&node.capability_needs)
        .bind(&node.budget_id)
        .bind(node.priority)
        .bind(Revision::INITIAL.get() as i64)
        .bind(node.thread_id.as_ref().map(ToString::to_string))
        .bind(node.origin.as_db_str())
        .execute(&mut **tx)
        .await?;
        self.get_node_tx(tx, &id).await
    }

    pub(crate) async fn get_node_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<WorkNode, GraphError> {
        Self::expect_prefix(id, Prefix::WorkNode)?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {NODE_COLUMNS} FROM work_nodes WHERE id = $1 AND tenant_id = $2"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => node_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::WorkNode.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn create_edge_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        edge: NewWorkEdge,
    ) -> Result<WorkEdge, GraphError> {
        Self::ensure_workspace_tx(tx, &self.tenant_id, &edge.workspace_id).await?;
        Self::expect_prefix(&edge.from_node_id, Prefix::WorkNode)?;
        Self::expect_prefix(&edge.to_node_id, Prefix::WorkNode)?;
        for endpoint in [&edge.from_node_id, &edge.to_node_id] {
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT id FROM work_nodes WHERE id = $1 AND tenant_id = $2 AND workspace_id = $3",
            )
            .bind(endpoint.to_string())
            .bind(&self.tenant_id)
            .bind(&edge.workspace_id)
            .fetch_optional(&mut **tx)
            .await?;
            if exists.is_none() {
                return Err(GraphError::NotFound {
                    entity: Entity::WorkNode.as_str(),
                    id: endpoint.to_string(),
                    tenant_id: self.tenant_id.clone(),
                });
            }
        }
        if edge.kind.is_acyclic_relation() {
            if edge.from_node_id == edge.to_node_id {
                return Err(GraphError::Cycle {
                    edge_kind: edge.kind.as_db_str(),
                    from: edge.from_node_id.to_string(),
                    to: edge.to_node_id.to_string(),
                });
            }
            if self
                .edge_would_cycle_tx(tx, &edge.workspace_id, &edge.from_node_id, &edge.to_node_id)
                .await?
            {
                return Err(GraphError::Cycle {
                    edge_kind: edge.kind.as_db_str(),
                    from: edge.from_node_id.to_string(),
                    to: edge.to_node_id.to_string(),
                });
            }
        }
        let id = self.generate_id(Prefix::WorkEdge);
        sqlx::query(
            "INSERT INTO work_edges (id, tenant_id, workspace_id, from_node_id, to_node_id, kind, \
             revision) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(&edge.workspace_id)
        .bind(edge.from_node_id.to_string())
        .bind(edge.to_node_id.to_string())
        .bind(edge.kind.as_db_str())
        .bind(Revision::INITIAL.get() as i64)
        .execute(&mut **tx)
        .await?;
        self.get_edge_tx(tx, &id).await
    }

    pub(crate) async fn get_edge_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<WorkEdge, GraphError> {
        Self::expect_prefix(id, Prefix::WorkEdge)?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {EDGE_COLUMNS} FROM work_edges WHERE id = $1 AND tenant_id = $2"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => edge_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::WorkEdge.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    pub(crate) async fn remove_edge_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        expected: Revision,
    ) -> Result<WorkEdge, GraphError> {
        let edge = self.get_edge_tx(tx, id).await?;
        if edge.revision != expected {
            return Err(Self::revision_conflict(
                Entity::WorkEdge,
                &id.to_string(),
                expected,
                edge.revision,
            ));
        }
        let deleted = sqlx::query(
            "DELETE FROM work_edges WHERE id = $1 AND tenant_id = $2 AND revision = $3",
        )
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(expected.get() as i64)
        .execute(&mut **tx)
        .await?;
        if deleted.rows_affected() != 1 {
            return Err(GraphError::StateConflict {
                entity: Entity::WorkEdge.as_str(),
                id: id.to_string(),
                expected: expected.to_string(),
            });
        }
        Ok(edge)
    }

    pub(crate) async fn transition_node_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        expected: Revision,
        to: WorkNodeStatus,
    ) -> Result<WorkNode, GraphError> {
        if to == WorkNodeStatus::Done {
            return Err(GraphError::VerificationRequired { id: id.to_string() });
        }
        self.apply_node_transition_tx(tx, id, expected, to).await
    }

    pub(crate) async fn mark_verification_passed_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        expected: Revision,
    ) -> Result<WorkNode, GraphError> {
        let current = self.lock_node_tx(tx, id).await?;
        if current.status != WorkNodeStatus::Verifying {
            return Err(GraphError::IllegalTransition {
                entity: Entity::WorkNode,
                from: current.status.as_db_str().to_string(),
                to: WorkNodeStatus::Done.as_db_str().to_string(),
            });
        }
        self.apply_node_transition_tx(tx, id, expected, WorkNodeStatus::Done)
            .await
    }

    pub(crate) async fn lock_node_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
    ) -> Result<WorkNode, GraphError> {
        Self::expect_prefix(id, Prefix::WorkNode)?;
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT {NODE_COLUMNS} FROM work_nodes WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
        ))
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row) => node_from_row(&row),
            None => Err(GraphError::NotFound {
                entity: Entity::WorkNode.as_str(),
                id: id.to_string(),
                tenant_id: self.tenant_id.clone(),
            }),
        }
    }

    async fn apply_node_transition_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        id: &CanonicalId,
        expected: Revision,
        to: WorkNodeStatus,
    ) -> Result<WorkNode, GraphError> {
        let current = self.lock_node_tx(tx, id).await?;
        if !current.status.can_transition_to(to) {
            return Err(GraphError::IllegalTransition {
                entity: Entity::WorkNode,
                from: current.status.as_db_str().to_string(),
                to: to.as_db_str().to_string(),
            });
        }
        let next = current.revision.check_and_bump(expected).map_err(|_| {
            Self::revision_conflict(
                Entity::WorkNode,
                &id.to_string(),
                expected,
                current.revision,
            )
        })?;
        let updated = sqlx::query(
            "UPDATE work_nodes SET status = $1, revision = $2 \
             WHERE id = $3 AND tenant_id = $4 AND revision = $5",
        )
        .bind(to.as_db_str())
        .bind(next.get() as i64)
        .bind(id.to_string())
        .bind(&self.tenant_id)
        .bind(expected.get() as i64)
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(GraphError::StateConflict {
                entity: Entity::WorkNode.as_str(),
                id: id.to_string(),
                expected: expected.to_string(),
            });
        }
        self.get_node_tx(tx, id).await
    }

    /// Recursive reachability check over the acyclic relation structure.
    ///
    /// Rejects the proposed `from -> to` edge when `to` already reaches `from` through
    /// existing `depends_on`/`parent_of` edges or through `work_nodes.parent_id`.
    async fn edge_would_cycle_tx(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        workspace_id: &str,
        from: &CanonicalId,
        to: &CanonicalId,
    ) -> Result<bool, GraphError> {
        let reaches: bool = sqlx::query_scalar(
            "WITH RECURSIVE reach(node_id) AS ( \
                 SELECT $1::text \
                 UNION \
                 SELECT link.next_id FROM ( \
                     SELECT e.from_node_id AS node_id, e.to_node_id AS next_id \
                       FROM work_edges e \
                      WHERE e.tenant_id = $2 AND e.workspace_id = $3 \
                        AND e.kind IN ('depends_on', 'parent_of') \
                     UNION ALL \
                     SELECT n.id AS node_id, n.parent_id AS next_id \
                       FROM work_nodes n \
                      WHERE n.tenant_id = $2 AND n.workspace_id = $3 AND n.parent_id IS NOT NULL \
                 ) link JOIN reach r ON link.node_id = r.node_id \
             ) SELECT EXISTS (SELECT 1 FROM reach WHERE node_id = $4)",
        )
        .bind(to.to_string())
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .bind(from.to_string())
        .fetch_one(&mut **tx)
        .await?;
        Ok(reaches)
    }
}
