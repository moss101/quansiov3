//! Authoritative persistence schema and migration runner (CORE-001).
//!
//! Canonical owner: `crates/server` control module (this path). The implementation is
//! `quansio_events::schema`, one layer down, so that stores below the server (the graph
//! store, the event log) share the same tenant-context primitive without a package cycle;
//! every server module keeps addressing it as `crate::control::schema`.
//!
//! Rollback policy: forward migrations with forward-fix (see `migrations/README.md`).
//! There is no automated `down`; recovery is a new forward migration plus restore.

pub use quansio_events::schema::{
    clear_tenant_context, clear_tenant_context_conn, exec, migrate, set_tenant_context,
    set_tenant_context_conn, set_user_context, validate_tenant_id, SchemaError,
    AUTHORITATIVE_TABLES, MIGRATIONS,
};
