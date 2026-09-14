//! Shared helpers for database-backed event-store tests.
//!
//! Every test creates and drops its own scratch database, so a development database is
//! never touched. `QUANSIO_TEST_POSTGRES_URL` is the superuser DSN used to create the
//! scratch database; when it is unset the tests report `BLOCKED_EXTERNAL` and return,
//! because the local development stack (`scripts/dev/up`) is not running.
//!
//! Migrations are applied from the canonical `migrations/` set through the same
//! `sqlx::migrate!` runner the control module uses, so this crate does not depend on a
//! second migrator (nor on `quansio-server`, which depends on this crate at runtime).
#![allow(dead_code)]

use quansio_core::{CorrelationId, EventId, TypedId, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventStore, EventType};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};

/// Superuser DSN, or `None` when the environment does not provide one.
#[must_use]
pub fn admin_url() -> Option<String> {
    std::env::var("QUANSIO_TEST_POSTGRES_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Report the standard blocked marker once.
pub fn blocked_marker() {
    eprintln!("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; dev stack not running");
}

fn scratch_url(admin: &str, database: &str) -> String {
    match admin.rsplit_once('/') {
        Some((base, _)) => format!("{base}/{database}"),
        None => format!("{admin}/{database}"),
    }
}

/// Create an empty scratch database, apply the canonical migrations and return a pool.
pub async fn fresh_migrated_database(name: &str) -> Option<PgPool> {
    let admin = admin_url()?;
    let admin_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin)
        .await
        .expect("connect to the admin database");
    admin_pool
        .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
        .await
        .expect("drop scratch database");
    admin_pool
        .execute(format!("CREATE DATABASE {name}").as_str())
        .await
        .expect("create scratch database");
    let url = scratch_url(&admin, name);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect to the scratch database");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("apply canonical migrations");
    Some(pool)
}

/// Close a pool and drop its scratch database.
pub async fn drop_pool(pool: &PgPool, name: &str) {
    let Some(admin) = admin_url() else {
        return;
    };
    // The pool is not closed first. `PgPool::close` waits for every checked-out connection to be
    // returned, and a test calling this is usually still holding one -- the connection is a local that
    // outlives its last query -- so closing first waits forever instead of finishing. `DROP DATABASE
    // ... WITH (FORCE)` terminates whatever sessions are still attached, so the database is gone either
    // way, and the pool is closed when its owner drops it.
    let _ = pool;
    if let Ok(admin_pool) = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin)
        .await
    {
        let _ = admin_pool
            .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
            .await;
    }
}

/// Unique scratch database name for a test.
#[must_use]
pub fn scratch_name(prefix: &str) -> String {
    format!("quansio_evt_{prefix}_{}", std::process::id())
}

/// Seed one tenant and one workspace inside it.
pub async fn seed_tenant(pool: &PgPool, tenant: &str, workspace: &str) {
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'seed')")
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("insert tenant");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'seed')")
        .bind(workspace)
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("insert workspace");
    tx.commit().await.expect("commit");
}

/// URL of an existing scratch database, or `None` when the environment provides none.
#[must_use]
pub fn database_url(name: &str) -> Option<String> {
    admin_url().map(|admin| scratch_url(&admin, name))
}

/// Reconnect to an existing scratch database (used to simulate a restarted process).
pub async fn connect_database(name: &str) -> Option<PgPool> {
    let url = database_url(name)?;
    Some(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect to the scratch database"),
    )
}

/// Collect frames until the session ends, failing the test on an unexpected error.
pub async fn collect_frames(
    session: &mut quansio_events::StreamSession,
) -> Vec<quansio_events::StreamFrame> {
    let mut frames = Vec::new();
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), session.next_frame()).await {
            Ok(Ok(frame)) => frames.push(frame),
            Ok(Err(quansio_events::StreamError::Closed)) => return frames,
            Ok(Err(other)) => panic!("unexpected stream error: {other}"),
            Err(_) => panic!("timed out waiting for a stream frame"),
        }
    }
}

/// One RuntimeEvent to commit in its own transaction.
pub struct SeedEvent<'a> {
    /// Aggregate kind, e.g. `run`.
    pub aggregate_type: &'a str,
    /// Aggregate identity.
    pub aggregate_id: &'a str,
    /// Version of the aggregate after the transition.
    pub aggregate_version: u64,
    /// Canonical `<family>.<event>` type.
    pub event_type: &'a str,
    /// Owning workspace, when the event is workspace-scoped.
    pub workspace_id: Option<&'a str>,
    /// Event payload.
    pub payload: serde_json::Value,
}

/// Commit one event (with a real state mutation) and return its event id.
pub async fn commit_event(store: &EventStore, tenant_id: &str, seed: SeedEvent<'_>) -> EventId {
    let mut generator = UlidGenerator::new();
    let correlation_id = CorrelationId::generate(&mut generator);
    let event_id = EventId::generate(&mut generator);
    let event_type = EventType::parse(seed.event_type).expect("canonical event type");
    let mut draft = EventDraft::new(
        seed.aggregate_type,
        seed.aggregate_id,
        seed.aggregate_version,
        event_type,
        correlation_id,
        Actor::system("projection_test"),
    )
    .with_event_id(event_id)
    .with_payload(seed.payload);
    if let Some(workspace_id) = seed.workspace_id {
        draft = draft.with_workspace(workspace_id);
    }
    let tenant = tenant_id.to_string();
    store
        .commit_mutation(tenant_id, move |conn, batch| {
            Box::pin(async move {
                sqlx::query("UPDATE tenants SET name = $2 WHERE id = $1")
                    .bind(&tenant)
                    .bind(format!("seed-{event_id}"))
                    .execute(&mut *conn)
                    .await?;
                batch.emit(draft);
                Ok(event_id)
            })
        })
        .await
        .expect("commit event");
    event_id
}
