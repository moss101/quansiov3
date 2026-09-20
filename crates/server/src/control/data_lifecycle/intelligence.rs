//! Entry-level forgetting for knowledge and memory (APP-011; the deletion workflow of
//! DOSSIER.md §21.3 that OPS-002 owns across authoritative and derived stores).
//!
//! A person's "forget this" is a data-lifecycle decision, not intelligence: the
//! authoritative row gets its tombstone (the §11.4 terminal `deleted` edge, or
//! `superseded` naming an existing successor), the entry's rows leave the derived
//! semantic index (`derived.embeddings`, keyed by source kind and ref exactly as the
//! intelligence plane keys them), and the `knowledge.*` / `memory.*` RuntimeEvent is
//! staged — all in one transaction, so the audit trail, the tombstone and the purge
//! cannot disagree. The row itself stays: provenance and audit survive a deletion.
//!
//! The lifecycle *model* is the intelligence plane's (`python/intelligence/knowledge/models.py`,
//! `python/intelligence/memory/models.py`); this module applies only the tombstone edges those
//! ladders have, and `python/tests/intelligence/test_data_lifecycle_edges.py` pins the edge
//! tables below against the models so the two cannot drift apart.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use quansio_core::{CanonicalId, CorrelationId, Prefix};
use quansio_events::event_type::EventType;
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;

/// Repository path of this module's canonical owner.
pub const DATA_LIFECYCLE_OWNER: &str = "crates/server/src/control/data_lifecycle";

/// `derived.embeddings.source_kind` for a knowledge entry (the plane's `SOURCE_KIND_KNOWLEDGE`).
pub const SOURCE_KIND_KNOWLEDGE: &str = "knowledge_entry";
/// `derived.embeddings.source_kind` for a memory entry (the plane's `SOURCE_KIND_MEMORY`).
pub const SOURCE_KIND_MEMORY: &str = "memory_entry";

/// Knowledge states from which `deleted` is a legal edge (DOMAIN.md §11.4).
pub const KNOWLEDGE_DELETE_FROM: [&str; 5] = [
    "candidate",
    "verified",
    "active",
    "superseded",
    "quarantined",
];
/// Knowledge states from which `superseded` is a legal edge (DOMAIN.md §11.4).
pub const KNOWLEDGE_SUPERSEDE_FROM: [&str; 2] = ["verified", "active"];
/// Memory states from which `deleted` is a legal edge (DOMAIN.md §11.4).
pub const MEMORY_DELETE_FROM: [&str; 2] = ["candidate", "active"];

/// Who is forgetting.
#[derive(Debug, Clone)]
pub struct DataAdmin {
    /// Owning tenant; every statement is scoped to it and RLS enforces it.
    pub tenant_id: String,
    /// Who acted.
    pub actor: Actor,
    /// Everything caused by one external input shares this id.
    pub correlation_id: CorrelationId,
}

impl DataAdmin {
    /// An administrative identity for a tenant.
    #[must_use]
    pub fn new(tenant_id: impl Into<String>, actor: Actor, correlation_id: CorrelationId) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            actor,
            correlation_id,
        }
    }
}

/// A data-lifecycle refusal.
#[derive(Debug, Error)]
pub enum DataLifecycleError {
    /// The entry is not in this tenant.
    #[error("{entity} {id} not found")]
    NotFound {
        /// `knowledge entry` or `memory entry`.
        entity: &'static str,
        /// Requested id.
        id: String,
    },
    /// The edge is not on the entry's ladder.
    #[error("{entity} {id} is {from} and cannot become {to} (legal only from {legal:?})")]
    IllegalTransition {
        /// `knowledge entry` or `memory entry`.
        entity: &'static str,
        /// The entry.
        id: String,
        /// Its current state.
        from: String,
        /// The refused state.
        to: &'static str,
        /// The states from which `to` is a legal edge.
        legal: Vec<&'static str>,
    },
    /// A parameter is not what the ladder needs.
    #[error("{0}")]
    Invalid(String),
    /// The entry reached the requested state concurrently, between the idempotency read
    /// and the row lock; the caller re-reads and reports the idempotent outcome.
    #[error("the entry changed concurrently; re-read it")]
    Raced,
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store refused the transaction that carried the mutation.
    #[error(transparent)]
    Event(#[from] EventError),
}

impl DataLifecycleError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "NOT_FOUND",
            Self::IllegalTransition { .. } => "CONFLICT_STATE",
            Self::Invalid(_) => "VALIDATION_SCHEMA",
            Self::Raced | Self::Database(_) | Self::Event(_) => "INTERNAL",
        }
    }
}

/// What a tombstone did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    /// The entry.
    pub id: String,
    /// Its workspace, when scoped to one.
    pub workspace_id: Option<String>,
    /// The state before this call.
    pub previous_status: String,
    /// The state after it.
    pub status: String,
    /// The successor, for a supersession.
    pub superseded_by: Option<String>,
    /// `derived.embeddings` rows removed by this call.
    pub derived_rows_removed: u64,
    /// The entry was already in `status`; nothing changed and no event was staged, but the
    /// purge still ran because an earlier attempt may not have reached the index.
    pub already: bool,
}

type BoxLifecycleFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DataLifecycleError>> + Send + 'a>>;

/// The forgetting path over `knowledge_entries`, `memory_entries` and `derived.embeddings`.
#[derive(Debug, Clone)]
pub struct DataLifecycle {
    events: EventStore,
}

struct Entry {
    id: String,
    workspace_id: Option<String>,
    status: String,
}

impl DataLifecycle {
    /// A control writing through `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            events: EventStore::new(pool),
        }
    }

    /// Tombstone a knowledge entry (`→ deleted`), purge its derived rows, stage
    /// `knowledge.deleted`.
    ///
    /// # Errors
    /// [`DataLifecycleError::NotFound`] for an unknown entry; the edge is legal from every
    /// non-deleted state, and an already deleted entry is left as it is (the purge still runs).
    pub async fn delete_knowledge(
        &self,
        admin: &DataAdmin,
        knowledge_id: &str,
    ) -> Result<Tombstone, DataLifecycleError> {
        let id = canonical(knowledge_id, Prefix::KnowledgeEntry, "knowledge entry")?;
        let target = Target {
            table: "knowledge_entries",
            entity: "knowledge entry",
            source_kind: SOURCE_KIND_KNOWLEDGE,
            status: "deleted",
        };
        if let Some(done) = self.already(&admin.tenant_id, &target, &id).await? {
            return Ok(done);
        }
        let tenant_id = admin.tenant_id.clone();
        let admin = admin.clone();
        let outcome = self
            .commit(&tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let entry = load(
                        tx,
                        &admin.tenant_id,
                        "knowledge_entries",
                        "knowledge entry",
                        &id,
                    )
                    .await?;
                    if entry.status == "deleted" {
                        return Err(DataLifecycleError::Raced);
                    }
                    legal(&entry, "knowledge entry", "deleted", &KNOWLEDGE_DELETE_FROM)?;
                    sqlx::query(
                        "UPDATE knowledge_entries SET status = 'deleted', updated_at = now() \
                     WHERE tenant_id = $1 AND id = $2 AND status = $3",
                    )
                    .bind(&admin.tenant_id)
                    .bind(&entry.id)
                    .bind(&entry.status)
                    .execute(&mut **tx)
                    .await?;
                    let removed =
                        purge(tx, &admin.tenant_id, SOURCE_KIND_KNOWLEDGE, &entry.id).await?;
                    stage(
                        &admin,
                        tx,
                        batch,
                        "knowledge",
                        &entry,
                        "knowledge.deleted",
                        json!({
                            "knowledge_id": entry.id,
                            "from": entry.status,
                            "to": "deleted",
                            "derived_rows_removed": removed,
                        }),
                    )
                    .await?;
                    Ok(Tombstone {
                        id: entry.id,
                        workspace_id: entry.workspace_id,
                        previous_status: entry.status,
                        status: "deleted".to_string(),
                        superseded_by: None,
                        derived_rows_removed: removed,
                        already: false,
                    })
                })
            })
            .await;
        self.settle(outcome, &tenant_id, &target, &id).await
    }

    /// Supersede a knowledge entry with an existing successor (`→ superseded`), purge its
    /// derived rows, stage `knowledge.superseded`.
    ///
    /// Supersession is not new content: the successor already exists as its own entry with
    /// its own provenance and lifecycle, and this call only points the old entry at it.
    ///
    /// # Errors
    /// [`DataLifecycleError::NotFound`] for an unknown entry or successor,
    /// [`DataLifecycleError::Invalid`] when the successor is the entry itself, and
    /// [`DataLifecycleError::IllegalTransition`] unless the entry is `verified` or `active`.
    pub async fn supersede_knowledge(
        &self,
        admin: &DataAdmin,
        knowledge_id: &str,
        superseded_by: &str,
    ) -> Result<Tombstone, DataLifecycleError> {
        let id = canonical(knowledge_id, Prefix::KnowledgeEntry, "knowledge entry")?;
        let successor = canonical(superseded_by, Prefix::KnowledgeEntry, "successor")?;
        if id == successor {
            return Err(DataLifecycleError::Invalid(
                "an entry cannot supersede itself".to_string(),
            ));
        }
        let admin = admin.clone();
        self.commit(&admin.tenant_id.clone(), move |tx, batch| {
            Box::pin(async move {
                let entry = load(
                    tx,
                    &admin.tenant_id,
                    "knowledge_entries",
                    "knowledge entry",
                    &id,
                )
                .await?;
                let next = load(
                    tx,
                    &admin.tenant_id,
                    "knowledge_entries",
                    "successor",
                    &successor,
                )
                .await?;
                legal(
                    &entry,
                    "knowledge entry",
                    "superseded",
                    &KNOWLEDGE_SUPERSEDE_FROM,
                )?;
                sqlx::query(
                    "UPDATE knowledge_entries SET status = 'superseded', superseded_by = $4, \
                     updated_at = now() WHERE tenant_id = $1 AND id = $2 AND status = $3",
                )
                .bind(&admin.tenant_id)
                .bind(&entry.id)
                .bind(&entry.status)
                .bind(&next.id)
                .execute(&mut **tx)
                .await?;
                let removed = purge(tx, &admin.tenant_id, SOURCE_KIND_KNOWLEDGE, &entry.id).await?;
                stage(
                    &admin,
                    tx,
                    batch,
                    "knowledge",
                    &entry,
                    "knowledge.superseded",
                    json!({
                        "knowledge_id": entry.id,
                        "from": entry.status,
                        "to": "superseded",
                        "superseded_by": next.id,
                        "derived_rows_removed": removed,
                    }),
                )
                .await?;
                Ok(Tombstone {
                    id: entry.id,
                    workspace_id: entry.workspace_id,
                    previous_status: entry.status,
                    status: "superseded".to_string(),
                    superseded_by: Some(next.id),
                    derived_rows_removed: removed,
                    already: false,
                })
            })
        })
        .await
    }

    /// Tombstone a memory entry (`→ deleted`), purge its derived rows, stage `memory.deleted`.
    ///
    /// # Errors
    /// [`DataLifecycleError::NotFound`] for an unknown entry; the edge is legal from every
    /// non-deleted state, and an already deleted entry is left as it is (the purge still runs).
    pub async fn delete_memory(
        &self,
        admin: &DataAdmin,
        memory_id: &str,
    ) -> Result<Tombstone, DataLifecycleError> {
        let id = canonical(memory_id, Prefix::MemoryEntry, "memory entry")?;
        let target = Target {
            table: "memory_entries",
            entity: "memory entry",
            source_kind: SOURCE_KIND_MEMORY,
            status: "deleted",
        };
        if let Some(done) = self.already(&admin.tenant_id, &target, &id).await? {
            return Ok(done);
        }
        let tenant_id = admin.tenant_id.clone();
        let admin = admin.clone();
        let outcome = self
            .commit(&tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let entry =
                        load(tx, &admin.tenant_id, "memory_entries", "memory entry", &id).await?;
                    if entry.status == "deleted" {
                        return Err(DataLifecycleError::Raced);
                    }
                    legal(&entry, "memory entry", "deleted", &MEMORY_DELETE_FROM)?;
                    sqlx::query(
                        "UPDATE memory_entries SET status = 'deleted', updated_at = now() \
                     WHERE tenant_id = $1 AND id = $2 AND status = $3",
                    )
                    .bind(&admin.tenant_id)
                    .bind(&entry.id)
                    .bind(&entry.status)
                    .execute(&mut **tx)
                    .await?;
                    let removed =
                        purge(tx, &admin.tenant_id, SOURCE_KIND_MEMORY, &entry.id).await?;
                    stage(
                        &admin,
                        tx,
                        batch,
                        "memory",
                        &entry,
                        "memory.deleted",
                        json!({
                            "memory_id": entry.id,
                            "from": entry.status,
                            "to": "deleted",
                            "derived_rows_removed": removed,
                        }),
                    )
                    .await?;
                    Ok(Tombstone {
                        id: entry.id,
                        workspace_id: entry.workspace_id,
                        previous_status: entry.status,
                        status: "deleted".to_string(),
                        superseded_by: None,
                        derived_rows_removed: removed,
                        already: false,
                    })
                })
            })
            .await;
        self.settle(outcome, &tenant_id, &target, &id).await
    }

    /// Run one mutation that must commit its event with it; the typed refusal travels out
    /// of band the same way the skill store's does.
    async fn commit<T, F>(&self, tenant_id: &str, mutation: F) -> Result<T, DataLifecycleError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxLifecycleFuture<'a, T>,
    {
        let rejection: Arc<Mutex<Option<DataLifecycleError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    match mutation(tx, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: DATA_LIFECYCLE_OWNER,
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(error) => match rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                Some(error) => Err(error),
                None => Err(DataLifecycleError::Event(error)),
            },
        }
    }
}

/// Which table, which derived source kind and which tombstone state a call is about.
struct Target {
    table: &'static str,
    entity: &'static str,
    source_kind: &'static str,
    status: &'static str,
}

impl DataLifecycle {
    /// The idempotent outcome when the entry is already in the tombstone state: nothing to
    /// stage, but the purge still runs because an earlier attempt may not have reached the
    /// index. `None` when the entry is not there yet.
    async fn already(
        &self,
        tenant_id: &str,
        target: &Target,
        id: &CanonicalId,
    ) -> Result<Option<Tombstone>, DataLifecycleError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let entry = load(&mut tx, tenant_id, target.table, target.entity, id).await?;
        if entry.status != target.status {
            tx.rollback().await?;
            return Ok(None);
        }
        let removed = purge(&mut tx, tenant_id, target.source_kind, &entry.id).await?;
        tx.commit().await?;
        Ok(Some(Tombstone {
            id: entry.id,
            workspace_id: entry.workspace_id,
            previous_status: entry.status.clone(),
            status: entry.status,
            superseded_by: None,
            derived_rows_removed: removed,
            already: true,
        }))
    }

    /// Resolve a race: the entry reached the state under someone else's transaction, so
    /// the outcome is the idempotent one.
    async fn settle(
        &self,
        outcome: Result<Tombstone, DataLifecycleError>,
        tenant_id: &str,
        target: &Target,
        id: &CanonicalId,
    ) -> Result<Tombstone, DataLifecycleError> {
        match outcome {
            Err(DataLifecycleError::Raced) => self
                .already(tenant_id, target, id)
                .await?
                .ok_or(DataLifecycleError::Raced),
            other => other,
        }
    }
}

fn canonical(value: &str, prefix: Prefix, what: &str) -> Result<CanonicalId, DataLifecycleError> {
    CanonicalId::parse_typed(value, prefix)
        .map_err(|error| DataLifecycleError::Invalid(format!("{what}: {error}")))
}

fn legal(
    entry: &Entry,
    entity: &'static str,
    to: &'static str,
    from: &[&'static str],
) -> Result<(), DataLifecycleError> {
    if from.contains(&entry.status.as_str()) {
        return Ok(());
    }
    Err(DataLifecycleError::IllegalTransition {
        entity,
        id: entry.id.clone(),
        from: entry.status.clone(),
        to,
        legal: from.to_vec(),
    })
}

/// Read one entry, taking the row lock the tombstone needs.
async fn load(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    table: &'static str,
    entity: &'static str,
    id: &CanonicalId,
) -> Result<Entry, DataLifecycleError> {
    let sql = format!(
        "SELECT id, workspace_id, status FROM {table} WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    );
    let row: Option<PgRow> = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    let row = row.ok_or_else(|| DataLifecycleError::NotFound {
        entity,
        id: id.to_string(),
    })?;
    Ok(Entry {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        status: row.try_get("status")?,
    })
}

/// Remove the entry's rows from the derived semantic index.
async fn purge(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    source_kind: &str,
    source_ref: &str,
) -> Result<u64, DataLifecycleError> {
    let done = sqlx::query(
        "DELETE FROM derived.embeddings WHERE tenant_id = $1 AND source_kind = $2 AND source_ref = $3",
    )
    .bind(tenant_id)
    .bind(source_kind)
    .bind(source_ref)
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected())
}

/// Stage one lifecycle event at the aggregate's next version.
async fn stage(
    admin: &DataAdmin,
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    aggregate_type: &str,
    entry: &Entry,
    event_type: &str,
    payload: Value,
) -> Result<(), DataLifecycleError> {
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 \
         AND aggregate_type = $2 AND aggregate_id = $3",
    )
    .bind(&admin.tenant_id)
    .bind(aggregate_type)
    .bind(&entry.id)
    .fetch_one(&mut **tx)
    .await?;
    let aggregate_version = u64::try_from(existing).unwrap_or(0).saturating_add(1);
    let mut draft = EventDraft::new(
        aggregate_type,
        &entry.id,
        aggregate_version,
        EventType::parse(event_type)?,
        admin.correlation_id,
        admin.actor.clone(),
    )
    .with_payload(payload);
    if let Some(workspace_id) = &entry.workspace_id {
        draft = draft.with_workspace(workspace_id.clone());
    }
    batch.emit(draft);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tombstone_edges_are_the_ladders_terminal_edges() {
        // Every knowledge state but `deleted` may be deleted; only in-force knowledge may be
        // superseded; every memory state but `deleted` may be deleted (DOMAIN.md §11.4).
        assert!(!KNOWLEDGE_DELETE_FROM.contains(&"deleted"));
        assert_eq!(KNOWLEDGE_DELETE_FROM.len(), 5);
        assert_eq!(KNOWLEDGE_SUPERSEDE_FROM, ["verified", "active"]);
        assert_eq!(MEMORY_DELETE_FROM, ["candidate", "active"]);
    }

    #[test]
    fn a_refused_edge_names_both_ends() {
        let entry = Entry {
            id: "kn_x".to_string(),
            workspace_id: None,
            status: "candidate".to_string(),
        };
        let error = legal(
            &entry,
            "knowledge entry",
            "superseded",
            &KNOWLEDGE_SUPERSEDE_FROM,
        )
        .expect_err("candidate cannot be superseded");
        assert_eq!(error.code(), "CONFLICT_STATE");
        assert!(error.to_string().contains("candidate"));
        assert!(error.to_string().contains("superseded"));
    }
}
