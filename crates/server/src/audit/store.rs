//! The durable, hash-chained audit log over `audit_entries` (OPS-002, DOMAIN.md §16).
//!
//! [`append_in`] writes one entry inside the caller's transaction, so an administrative
//! decision and its audit record commit together or not at all: the tenant's chain is
//! serialized with a transaction-scoped advisory lock, the new entry's `prev_hash` is the
//! chain tail's `hash`, and `hash` covers the canonical fields of [`AuditEntry`] — the
//! same canonical form the in-memory model uses, so [`verify`] can recompute the chain
//! from the rows alone. Entries are never updated or deleted here; there is no such
//! function, and RUN-006's row-level security scopes every read to the tenant.

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::{digest, AuditEntry};
use crate::control::schema;

/// What an owner records; identity, chain and time are the store's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditDraft {
    /// The workspace the decision was scoped to, if any.
    pub workspace_id: Option<String>,
    /// Who decided (`{kind, id}` as the event envelope spells an actor).
    pub actor: Value,
    /// The command name, ops action or policy decision.
    pub action: String,
    /// What it was about (`membership:tm_…`, `policy:pol_…`, `approval:apr_…`).
    pub target_ref: Option<String>,
    /// `applied` | `granted` | `denied` | `refused` …
    pub decision: Option<String>,
    /// A one-line reason.
    pub reason: Option<String>,
    /// The request's correlation id.
    pub correlation_id: Option<String>,
}

/// One row as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// The chained entry.
    pub entry: AuditEntry,
    /// Actor as stored.
    pub actor_json: Value,
    /// When, as stored.
    pub occurred_at: DateTime<Utc>,
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Append one entry to the tenant's chain inside `tx`.
///
/// # Errors
/// Returns the database error; the caller's transaction is then rolled back by the caller.
pub async fn append_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    draft: AuditDraft,
) -> Result<AuditEntry, sqlx::Error> {
    // One writer per tenant chain at a time, released with the transaction.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('audit:' || $1))")
        .bind(tenant_id)
        .execute(&mut **tx)
        .await?;
    let tail: Option<String> = sqlx::query_scalar(
        "SELECT hash FROM audit_entries WHERE tenant_id = $1 \
         ORDER BY occurred_at DESC, id DESC LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    // PostgreSQL keeps microseconds; hash what will be read back.
    let occurred = Utc::now();
    let occurred = occurred
        .with_nanosecond((occurred.nanosecond() / 1_000) * 1_000)
        .unwrap_or(occurred);
    let mut entry = AuditEntry {
        id: CanonicalId::generate(Prefix::AuditEntry, &mut UlidGenerator::new()).to_string(),
        tenant_id: tenant_id.to_string(),
        workspace_id: draft.workspace_id,
        actor: draft.actor.to_string(),
        action: draft.action,
        target_ref: draft.target_ref.unwrap_or_default(),
        decision: draft.decision.unwrap_or_default(),
        reason: draft.reason.unwrap_or_default(),
        correlation_id: draft.correlation_id.unwrap_or_default(),
        occurred_at: rfc3339(occurred),
        prev_hash: tail.unwrap_or_default(),
        hash: String::new(),
    };
    entry.hash = digest(&entry.canonical());
    sqlx::query(
        "INSERT INTO audit_entries (id, tenant_id, workspace_id, actor, action, target_ref, decision, \
                                    reason, correlation_id, occurred_at, prev_hash, hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(&entry.id)
    .bind(tenant_id)
    .bind(&entry.workspace_id)
    .bind(&draft.actor)
    .bind(&entry.action)
    .bind(empty_as_null(&entry.target_ref))
    .bind(empty_as_null(&entry.decision))
    .bind(empty_as_null(&entry.reason))
    .bind(empty_as_null(&entry.correlation_id))
    .bind(occurred)
    .bind(empty_as_null(&entry.prev_hash))
    .bind(&entry.hash)
    .execute(&mut **tx)
    .await?;
    Ok(entry)
}

fn empty_as_null(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn record(row: &PgRow) -> Result<AuditRecord, sqlx::Error> {
    let actor_json: Value = row.try_get("actor")?;
    let occurred_at: DateTime<Utc> = row.try_get("occurred_at")?;
    Ok(AuditRecord {
        entry: AuditEntry {
            id: row.try_get("id")?,
            tenant_id: row.try_get("tenant_id")?,
            workspace_id: row.try_get("workspace_id")?,
            actor: actor_json.to_string(),
            action: row.try_get("action")?,
            target_ref: row
                .try_get::<Option<String>, _>("target_ref")?
                .unwrap_or_default(),
            decision: row
                .try_get::<Option<String>, _>("decision")?
                .unwrap_or_default(),
            reason: row
                .try_get::<Option<String>, _>("reason")?
                .unwrap_or_default(),
            correlation_id: row
                .try_get::<Option<String>, _>("correlation_id")?
                .unwrap_or_default(),
            occurred_at: rfc3339(occurred_at),
            prev_hash: row
                .try_get::<Option<String>, _>("prev_hash")?
                .unwrap_or_default(),
            hash: row.try_get("hash")?,
        },
        actor_json,
        occurred_at,
    })
}

const COLUMNS: &str = "id, tenant_id, workspace_id, actor, action, target_ref, decision, reason, \
                       correlation_id, occurred_at, prev_hash, hash";

/// The tenant's entries, newest first, optionally one workspace's, at most `limit`.
///
/// # Errors
/// Returns the database error.
pub async fn list(
    pool: &PgPool,
    tenant_id: &str,
    workspace_id: Option<&str>,
    limit: i64,
) -> Result<Vec<AuditRecord>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM audit_entries WHERE tenant_id = $1 \
         AND ($2::text IS NULL OR workspace_id = $2) \
         ORDER BY occurred_at DESC, id DESC LIMIT $3"
    ))
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    rows.iter().map(record).collect()
}

/// The outcome of walking a tenant's chain from its genesis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainCheck {
    /// Entries walked.
    pub entries: usize,
    /// The first entry whose `prev_hash` or `hash` does not fit, if any.
    pub broken_at: Option<String>,
}

impl ChainCheck {
    /// Whether every link holds.
    #[must_use]
    pub const fn holds(&self) -> bool {
        self.broken_at.is_none()
    }
}

/// Recompute the tenant's chain from the rows.
///
/// # Errors
/// Returns the database error.
pub async fn verify(pool: &PgPool, tenant_id: &str) -> Result<ChainCheck, sqlx::Error> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM audit_entries WHERE tenant_id = $1 ORDER BY occurred_at ASC, id ASC"
    ))
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut prev = String::new();
    let mut entries = 0;
    for row in &rows {
        let found = record(row)?;
        entries += 1;
        if found.entry.prev_hash != prev || digest(&found.entry.canonical()) != found.entry.hash {
            return Ok(ChainCheck {
                entries,
                broken_at: Some(found.entry.id),
            });
        }
        prev = found.entry.hash;
    }
    Ok(ChainCheck {
        entries,
        broken_at: None,
    })
}
