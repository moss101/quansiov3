//! Schema bootstrap, tenant isolation and migration-policy tests (CORE-001).
//!
//! These tests drive the shipped migration set (`migrations/`) through the shipped
//! runner (`quansio_server::control::schema`) against a real PostgreSQL server. They
//! create and drop a scratch database, so they never touch a development database.
//!
//! Environment (documented per AGENTS.md "Real boundaries"):
//!   `QUANSIO_TEST_POSTGRES_URL` — superuser DSN used to create the scratch database,
//!   for example `postgres://quansio:…@127.0.0.1:55440/postgres`. When it is absent the
//!   tests print an explicit `BLOCKED_EXTERNAL` marker and return, because the local
//!   development stack (`scripts/dev/up`) is not running.
//!
//! The `derived` schema, RLS policies and updated_at triggers are asserted directly:
//! a schema that silently loses them would otherwise only fail much later.

use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};

use quansio_server::control::schema;

const TENANT_A: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";
const USER_A: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORKSPACE_A: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORK_NODE_A: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORK_NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";

fn admin_url() -> Option<String> {
    std::env::var("QUANSIO_TEST_POSTGRES_URL")
        .ok()
        .filter(|v| !v.is_empty())
}

fn scratch_url(admin: &str, database: &str) -> String {
    match admin.rsplit_once('/') {
        Some((base, _)) => format!("{base}/{database}"),
        None => format!("{admin}/{database}"),
    }
}

async fn admin_pool(admin: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(admin)
        .await
        .expect("connect to the admin database")
}

/// Create an empty scratch database and return a pool bound to it.
async fn fresh_database(name: &str) -> Option<PgPool> {
    let admin = admin_url()?;
    let admin = admin_pool(&admin).await;
    admin
        .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
        .await
        .expect("drop scratch database");
    admin
        .execute(format!("CREATE DATABASE {name}").as_str())
        .await
        .expect("create scratch database");
    let url = scratch_url(&admin_url().expect("admin url"), name);
    Some(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect scratch db"),
    )
}

async fn drop_pool(pool: &PgPool, name: &str) {
    let admin = admin_url().expect("admin url");
    pool.close().await;
    let admin = admin_pool(&admin).await;
    let _ = admin
        .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
        .await;
}

async fn seed_tenant_a(pool: &PgPool) {
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'A')")
        .bind(TENANT_A)
        .execute(&mut *tx)
        .await
        .expect("insert tenant A");
    sqlx::query(
        "INSERT INTO users (id, primary_email, display_name) VALUES ($1, 'a@example.com', 'A')",
    )
    .bind(USER_A)
    .execute(&mut *tx)
    .await
    .expect("insert user A");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'WS')")
        .bind(WORKSPACE_A)
        .bind(TENANT_A)
        .execute(&mut *tx)
        .await
        .expect("insert workspace");
    schema::set_tenant_context(&mut tx, TENANT_A)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) \
         VALUES ($1, $2, $3, 'objective', 'node A', '{\"kind\":\"user\",\"id\":\"usr_x\"}'::jsonb)",
    )
    .bind(WORK_NODE_A)
    .bind(TENANT_A)
    .bind(WORKSPACE_A)
    .execute(&mut *tx)
    .await
    .expect("insert work node");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn bootstrap_from_zero_creates_the_authoritative_schema() {
    let Some(name) = admin_url().map(|_| format!("quansio_schema_{}", std::process::id())) else {
        eprintln!("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; dev stack not running");
        return;
    };
    let Some(pool) = fresh_database(&name).await else {
        eprintln!("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set");
        return;
    };

    schema::migrate(&pool).await.expect("migrate from zero");

    // Every canonical table exists.
    for table in schema::AUTHORITATIVE_TABLES {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .expect("table lookup");
        assert!(exists, "canonical table {table} is missing after migration");
    }

    // Rebuildable structures live in the derived schema, not among authoritative tables.
    let derived: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'derived'",
    )
    .fetch_one(&pool)
    .await
    .expect("derived tables");
    assert!(
        derived >= 2,
        "derived schema must hold the rebuildable structures"
    );

    // RLS is enabled and forced on every tenant table, and each has a policy.
    let unforced: i64 = sqlx::query_scalar(
        // `_sqlx_migrations` is the migrator's own bookkeeping table, not tenant data.
        "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname IN ('public', 'derived') AND c.relkind = 'r' \
           AND c.relname <> '_sqlx_migrations' AND NOT (c.relrowsecurity AND c.relforcerowsecurity)",
    )
    .fetch_one(&pool)
    .await
    .expect("rls check");
    assert_eq!(unforced, 0, "every table must have RLS enabled and forced");

    // updated_at is maintained by triggers, not by application code.
    let triggers: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_trigger WHERE NOT tgisinternal")
            .fetch_one(&pool)
            .await
            .expect("triggers");
    assert!(
        triggers >= 50,
        "updated_at triggers are missing: {triggers}"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn tenant_isolation_returns_zero_rows_without_context() {
    let Some(name) = admin_url().map(|_| format!("quansio_isolation_{}", std::process::id()))
    else {
        eprintln!("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; dev stack not running");
        return;
    };
    let Some(pool) = fresh_database(&name).await else {
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant_a(&pool).await;

    // The application role is not the table owner and not a superuser, so RLS applies.
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("SET LOCAL ROLE quansio_app")
        .execute(&mut *tx)
        .await
        .expect("set role");

    // With the tenant context set, the row is visible.
    schema::set_tenant_context(&mut tx, TENANT_A)
        .await
        .expect("context A");
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM work_nodes")
        .fetch_one(&mut *tx)
        .await
        .expect("count with context");
    assert_eq!(visible, 1, "tenant A must see its own work node");

    // A different tenant sees nothing.
    schema::set_tenant_context(&mut tx, TENANT_B)
        .await
        .expect("context B");
    let other: i64 = sqlx::query_scalar("SELECT count(*) FROM work_nodes")
        .fetch_one(&mut *tx)
        .await
        .expect("count as tenant B");
    assert_eq!(other, 0, "tenant B must not see tenant A rows");

    // No context fails closed.
    schema::clear_tenant_context(&mut tx)
        .await
        .expect("clear context");
    let unset: i64 = sqlx::query_scalar("SELECT count(*) FROM work_nodes")
        .fetch_one(&mut *tx)
        .await
        .expect("count without context");
    assert_eq!(
        unset, 0,
        "a query without tenant context must return zero rows"
    );

    // Writing without context is refused by the WITH CHECK clause.
    let refused = sqlx::query(
        "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) \
         VALUES ($1, $2, $3, 'task', 'smuggled', '{}'::jsonb)",
    )
    .bind(WORK_NODE_B)
    .bind(TENANT_A)
    .bind(WORKSPACE_A)
    .execute(&mut *tx)
    .await;
    assert!(
        refused.is_err(),
        "an insert without tenant context must be rejected"
    );

    tx.rollback().await.expect("rollback");
    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn migrations_are_reapplied_safely_over_existing_data() {
    let Some(name) = admin_url().map(|_| format!("quansio_forwardfix_{}", std::process::id()))
    else {
        eprintln!("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; dev stack not running");
        return;
    };
    let Some(pool) = fresh_database(&name).await else {
        return;
    };

    // "Supported legacy state" is an empty database until REL-001 ships a release.
    schema::migrate(&pool).await.expect("bootstrap");
    seed_tenant_a(&pool).await;

    // A forward-fix run (the rollback strategy) must be a no-op that preserves data.
    schema::migrate(&pool).await.expect("forward-fix rerun");

    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT_A)
        .await
        .expect("context");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM work_nodes")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    assert_eq!(count, 1, "forward-fix must preserve existing rows");
    tx.rollback().await.expect("rollback");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn tenant_context_validation_rejects_non_canonical_ids() {
    for invalid in [
        "",
        "tenant",
        "tn_lowercase",
        "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    ] {
        assert!(
            schema::validate_tenant_id(invalid).is_err(),
            "invalid tenant id {invalid:?} must be rejected"
        );
    }
    assert!(schema::validate_tenant_id(TENANT_A).is_ok());
}

#[tokio::test]
async fn unset_context_fails_closed_on_a_plain_connection() {
    let Some(name) = admin_url().map(|_| format!("quansio_failclosed_{}", std::process::id()))
    else {
        eprintln!("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; dev stack not running");
        return;
    };
    let Some(pool) = fresh_database(&name).await else {
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant_a(&pool).await;

    // Even outside an explicit transaction, forgetting the context is fail-closed.
    let mut conn = pool.acquire().await.expect("acquire");
    conn.execute("SET ROLE quansio_app")
        .await
        .expect("set role");
    let row = sqlx::query("SELECT count(*) AS n FROM work_nodes")
        .fetch_one(&mut *conn)
        .await
        .expect("count");
    assert_eq!(row.get::<i64, _>("n"), 0);

    drop_pool(&pool, &name).await;
}
