//! The shared store core: tenant context, identity generation and revision heads.
//!
//! `GraphStore` is the single entry point to the WorkGraph, AgentGraph and StateGraph
//! tables. Every method opens a transaction, sets the tenant context with
//! `quansio_server::control::schema::set_tenant_context`, and additionally scopes every
//! statement by `tenant_id`, so a missing or wrong tenant context can never leak a row
//! (DOSSIER.md §9, §16).
//!
//! The workspace graph revision head (`graph_heads`, migration 0002) is the aggregate
//! revision of DOMAIN.md §1.2: single-graph mutations advance it, and
//! [`crate::GraphStore::apply_batch`] compare-and-sets it once for a whole batch.

use std::sync::Mutex;

use quansio_core::{CanonicalId, Prefix, Revision, Ulid, UlidGenerator};
use sqlx::{Postgres, Transaction};

use crate::error::{Entity, GraphError};

/// Handle to the authoritative graph stores for one tenant.
pub struct GraphStore {
    pub(crate) pool: sqlx::PgPool,
    pub(crate) tenant_id: String,
    pub(crate) ids: Mutex<UlidGenerator>,
}

impl GraphStore {
    /// Bind a store to one tenant.
    ///
    /// # Errors
    /// Returns [`GraphError::TenantScope`] when `tenant_id` is not a canonical `tn_` id.
    pub fn new(pool: sqlx::PgPool, tenant_id: impl Into<String>) -> Result<Self, GraphError> {
        let tenant_id = tenant_id.into();
        quansio_server::control::schema::validate_tenant_id(&tenant_id)?;
        Ok(Self {
            pool,
            tenant_id,
            ids: Mutex::new(UlidGenerator::new()),
        })
    }

    /// The underlying pool, for callers that need to compose their own transaction.
    #[must_use]
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    /// The tenant every statement in this store is scoped to.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Generate a canonical id with the given entity prefix.
    pub(crate) fn generate_id(&self, prefix: Prefix) -> CanonicalId {
        let mut generator = self
            .ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        CanonicalId::generate(prefix, &mut generator)
    }

    /// Generate a fresh ULID, for schema ids that are not in the canonical prefix table
    /// (`agent_graph_edges.id` is `age_<ULID>` in `0001_canonical_schema.sql`).
    pub(crate) fn generate_ulid(&self) -> Ulid {
        let mut generator = self
            .ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        generator.generate()
    }

    /// Open a transaction with the tenant context already set.
    pub(crate) async fn begin(&self) -> Result<Transaction<'static, Postgres>, GraphError> {
        let mut tx = self.pool.begin().await?;
        quansio_server::control::schema::set_tenant_context(&mut tx, &self.tenant_id).await?;
        Ok(tx)
    }

    /// Reject an id whose prefix is not the one the caller requires.
    pub(crate) fn expect_prefix(id: &CanonicalId, prefix: Prefix) -> Result<(), GraphError> {
        if id.prefix() == prefix {
            return Ok(());
        }
        Err(GraphError::InvalidId {
            value: id.to_string(),
            expected: prefix.as_str(),
        })
    }

    /// Parse an optional id column, enforcing its prefix.
    pub(crate) fn optional_id(
        value: Option<String>,
        prefix: Prefix,
    ) -> Result<Option<CanonicalId>, GraphError> {
        match value {
            None => Ok(None),
            Some(raw) => Ok(Some(CanonicalId::parse_typed(&raw, prefix).map_err(
                |_| GraphError::InvalidId {
                    value: raw,
                    expected: prefix.as_str(),
                },
            )?)),
        }
    }

    /// Verify that a workspace exists inside this tenant before it is referenced.
    pub(crate) async fn ensure_workspace_tx(
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
        workspace_id: &str,
    ) -> Result<(), GraphError> {
        let visible: Option<String> =
            sqlx::query_scalar("SELECT id FROM workspaces WHERE id = $1 AND tenant_id = $2")
                .bind(workspace_id)
                .bind(tenant_id)
                .fetch_optional(&mut **tx)
                .await?;
        if visible.is_none() {
            return Err(GraphError::NotFound {
                entity: Entity::Workspace.as_str(),
                id: workspace_id.to_string(),
                tenant_id: tenant_id.to_string(),
            });
        }
        Ok(())
    }

    /// Lock (creating if needed) the workspace graph revision head.
    pub(crate) async fn ensure_head_tx(
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
        workspace_id: &str,
    ) -> Result<Revision, GraphError> {
        sqlx::query(
            "INSERT INTO graph_heads (tenant_id, workspace_id, revision) VALUES ($1, $2, 1) \
             ON CONFLICT (tenant_id, workspace_id) DO NOTHING",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .execute(&mut **tx)
        .await?;
        let revision: i64 = sqlx::query_scalar(
            "SELECT revision FROM graph_heads WHERE tenant_id = $1 AND workspace_id = $2 \
             FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .fetch_one(&mut **tx)
        .await?;
        Ok(Revision::new(revision as u64))
    }

    /// Advance the workspace graph revision head by one.
    pub(crate) async fn bump_head_tx(
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
        workspace_id: &str,
    ) -> Result<Revision, GraphError> {
        Self::ensure_head_tx(tx, tenant_id, workspace_id).await?;
        let revision: i64 = sqlx::query_scalar(
            "UPDATE graph_heads SET revision = revision + 1 \
             WHERE tenant_id = $1 AND workspace_id = $2 RETURNING revision",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .fetch_one(&mut **tx)
        .await?;
        Ok(Revision::new(revision as u64))
    }

    /// Read the current workspace graph revision without mutating it.
    ///
    /// # Errors
    /// Returns a [`GraphError`] when the head cannot be read.
    pub async fn graph_revision(&self, workspace_id: &str) -> Result<Revision, GraphError> {
        let mut tx = self.begin().await?;
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM graph_heads WHERE tenant_id = $1 AND workspace_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(revision.map_or(Revision::INITIAL, |value| Revision::new(value as u64)))
    }

    /// Translate a stale-generation result from `quansio_core::Revision` into a graph
    /// conflict naming the aggregate that was stale.
    pub(crate) fn revision_conflict(
        entity: Entity,
        id: &str,
        expected: Revision,
        current: Revision,
    ) -> GraphError {
        GraphError::RevisionConflict {
            entity: entity.as_str(),
            id: id.to_string(),
            expected: expected.get(),
            current: current.get(),
        }
    }
}
