//! Shared scratch-database helpers for the database-backed indexer tests.
//!
//! Each test creates and drops its own scratch database, so the shared development
//! database is never touched. `QUANSIO_TEST_POSTGRES_URL` is the superuser DSN used
//! to create the scratch database; when it is unset the tests print `BLOCKED_EXTERNAL`
//! and return, because the local development stack (`scripts/dev/up`) is not running.
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
    let url = join_url(&admin, name);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect to the scratch database");
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("apply migrations");
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
    format!("quansio_idx_{prefix}_{}", std::process::id())
}

fn join_url(admin: &str, database: &str) -> String {
    match admin.rsplit_once('/') {
        Some((base, _)) => format!("{base}/{database}"),
        None => format!("{admin}/{database}"),
    }
}

/// Seed a tenant, workspace, artifact and one artifact version row.
#[allow(clippy::too_many_arguments)]
pub async fn seed_artifact(
    pool: &PgPool,
    tenant: &str,
    workspace: &str,
    artifact: &str,
    version: &str,
    title: &str,
    object_key: &str,
    content_digest: &str,
    media_type: &str,
) {
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'seed') ON CONFLICT (id) DO NOTHING")
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("insert tenant");
    sqlx::query(
        "INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'seed') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(workspace)
    .bind(tenant)
    .execute(&mut *tx)
    .await
    .expect("insert workspace");
    sqlx::query(
        "INSERT INTO artifacts (id, tenant_id, workspace_id, kind, title, origin) \
         VALUES ($1, $2, $3, 'document', $4, '{\"kind\":\"upload\",\"ref\":\"seed\"}'::jsonb) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(artifact)
    .bind(tenant)
    .bind(workspace)
    .bind(title)
    .execute(&mut *tx)
    .await
    .expect("insert artifact");
    sqlx::query(
        "INSERT INTO artifact_versions \
         (id, tenant_id, artifact_id, seq, content_digest, size_bytes, media_type, object_key) \
         VALUES ($1, $2, $3, 1, $4, 0, $5, $6)",
    )
    .bind(version)
    .bind(tenant)
    .bind(artifact)
    .bind(content_digest)
    .bind(media_type)
    .bind(object_key)
    .execute(&mut *tx)
    .await
    .expect("insert artifact version");
    tx.commit().await.expect("commit");
}
