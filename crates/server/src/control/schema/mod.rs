//! Authoritative persistence schema and migration runner (CORE-001).
//!
//! Canonical owner: `crates/server` control module (`crates/server/src/control/schema/`).
//! The SQL migrations under `migrations/` are the single definition of the
//! authoritative control/runtime data model (DOMAIN.md §2–§13); this module applies
//! them and provides the tenant-context primitive that makes row-level security
//! effective.
//!
//! Rollback policy: forward migrations with forward-fix (see `migrations/README.md`).
//! There is no automated `down`; recovery is a new forward migration plus restore.

use sqlx::postgres::PgPool;
use sqlx::{Executor, Postgres, Transaction};

/// The canonical migration set: every file in `migrations/`, applied in order.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Schema errors surfaced to the control plane.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// The database rejected or could not apply a migration.
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A tenant context could not be established; callers must fail closed.
    #[error("tenant context failed: {0}")]
    TenantContext(#[from] sqlx::Error),
    /// The supplied tenant id is not a canonical `tn_` identifier.
    #[error("invalid tenant id: {0}")]
    InvalidTenantId(String),
}

/// Apply every pending migration. Safe to run repeatedly and concurrently.
pub async fn migrate(pool: &PgPool) -> Result<(), SchemaError> {
    MIGRATIONS.run(pool).await?;
    Ok(())
}

/// Set the tenant context for the current transaction.
///
/// Row-level security reads `current_setting('quansio.tenant_id', true)`; with no
/// context the setting is `NULL` and every tenant table returns zero rows, so an
/// unset context fails closed rather than leaking data (DOSSIER.md §16).
pub async fn set_tenant_context(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
) -> Result<(), SchemaError> {
    validate_tenant_id(tenant_id)?;
    sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
        .bind(tenant_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Set the acting user for the current transaction (used by `users` policies).
pub async fn set_user_context(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
) -> Result<(), SchemaError> {
    sqlx::query("SELECT set_config('quansio.user_id', $1, true)")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Clear the tenant context for the current transaction.
pub async fn clear_tenant_context(tx: &mut Transaction<'_, Postgres>) -> Result<(), SchemaError> {
    sqlx::query("SELECT set_config('quansio.tenant_id', '', true)")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Validate the canonical `tn_<ULID>` shape before it is used as a context value.
pub fn validate_tenant_id(tenant_id: &str) -> Result<(), SchemaError> {
    const ULID_LEN: usize = 26;
    let valid = tenant_id.starts_with("tn_")
        && tenant_id.len() == 3 + ULID_LEN
        && tenant_id[3..]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err(SchemaError::InvalidTenantId(tenant_id.to_string()))
    }
}

/// Tables the schema guarantees exist, used by bootstrap verification.
pub const AUTHORITATIVE_TABLES: &[&str] = &[
    "tenants",
    "users",
    "workspaces",
    "work_nodes",
    "work_edges",
    "agent_threads",
    "runs",
    "turns",
    "steps",
    "attempts",
    "runtime_events",
    "event_outbox",
    "effect_records",
    "approval_requests",
    "approval_receipts",
    "capability_projections",
    "protocol_states",
    "artifacts",
    "evidence",
    "knowledge_entries",
    "memory_entries",
    "teammates",
    "routines",
    "usage_records",
];

/// Execute a statement on the pool (thin helper used by bootstrap checks).
pub async fn exec<'e, E>(executor: E, sql: &str) -> Result<(), sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(sql).execute(executor).await?;
    Ok(())
}
