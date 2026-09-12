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
    pool.close().await;
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
