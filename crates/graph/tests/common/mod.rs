//! Shared helpers for the graph store integration tests.
//!
//! Every test creates and drops its own scratch database, so a development database is
//! never touched. `QUANSIO_TEST_POSTGRES_URL` is the DSN used to create the scratch
//! database; when it is unset the tests report `BLOCKED_EXTERNAL` and return, because the
//! local development stack (`scripts/dev/up`) is not running.

#![allow(dead_code)]

use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};

/// Tenant id used by fixtures.
pub const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
/// Second tenant id used by isolation fixtures.
pub const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
/// User id used by fixtures.
pub const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
/// Second user id used by isolation fixtures.
pub const USER_B: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
/// Workspace id used by fixtures.
pub const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
/// Second workspace id used by isolation fixtures.
pub const WORKSPACE_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
/// Seed WorkNode id.
pub const SEED_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
/// Second seed WorkNode id.
pub const SEED_NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";

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

/// Create an empty scratch database and return a pool bound to it.
pub async fn fresh_database(name: &str) -> Option<PgPool> {
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
    Some(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect to the scratch database"),
    )
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
    format!("quansio_graph_{prefix}_{}", std::process::id())
}

/// Seed one tenant, user, workspace and work node.
pub async fn seed_tenant(pool: &PgPool, tenant: &str, user: &str, workspace: &str, node: &str) {
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'seed')")
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("insert tenant");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'seed')")
        .bind(user)
        .bind(format!("{user}@example.com"))
        .execute(&mut *tx)
        .await
        .expect("insert user");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'seed')")
        .bind(workspace)
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("insert workspace");
    quansio_server::control::schema::set_tenant_context(&mut tx, tenant)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) \
         VALUES ($1, $2, $3, 'objective', 'seed node', '{\"kind\":\"user\",\"id\":\"usr_seed\"}'::jsonb)",
    )
    .bind(node)
    .bind(tenant)
    .bind(workspace)
    .execute(&mut *tx)
    .await
    .expect("insert work node");
    tx.commit().await.expect("commit");
}

/// Migrate a fresh scratch database and seed tenant A plus tenant B.
///
/// Returns `None` when `QUANSIO_TEST_POSTGRES_URL` is unset.
pub async fn prepare_two_tenants(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, SEED_NODE).await;
    seed_tenant(&pool, TENANT_B, USER_B, WORKSPACE_B, SEED_NODE_B).await;
    Some((name, pool))
}

/// Migrate a fresh scratch database and seed tenant A only.
pub async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, SEED_NODE).await;
    Some((name, pool))
}
