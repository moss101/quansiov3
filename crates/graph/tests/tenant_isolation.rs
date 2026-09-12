//! Tenant isolation: every graph read and write is scoped to the store's tenant (CORE-004).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare_two_tenants, TENANT, TENANT_B, WORKSPACE};
use quansio_graph::{GraphError, GraphStore, NewWorkNode, WorkNodeKind, WorkNodeStatus};
use sqlx::Row;

fn actor() -> serde_json::Value {
    serde_json::json!({"kind": "user", "id": "usr_seed"})
}

#[tokio::test]
async fn cross_tenant_reads_and_writes_return_nothing() {
    let Some((name, pool)) = prepare_two_tenants("isolation").await else {
        blocked_marker();
        return;
    };
    let tenant_a = GraphStore::new(pool.clone(), TENANT).expect("store a");
    let tenant_b = GraphStore::new(pool.clone(), TENANT_B).expect("store b");

    let node = tenant_a
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "tenant a only",
            actor(),
        ))
        .await
        .expect("create node");

    // Reads are scoped: tenant B cannot see tenant A's node or workspace listing.
    let error = tenant_b
        .get_node(&node.id)
        .await
        .expect_err("cross-tenant read");
    assert!(matches!(error, GraphError::NotFound { .. }), "{error:?}");
    assert!(tenant_b
        .list_nodes(WORKSPACE)
        .await
        .expect("list")
        .is_empty());

    // Writes are scoped: tenant B cannot mutate or remove tenant A's node.
    let error = tenant_b
        .transition_node(&node.id, node.revision, WorkNodeStatus::Ready)
        .await
        .expect_err("cross-tenant write");
    assert!(matches!(error, GraphError::NotFound { .. }), "{error:?}");
    assert_eq!(
        tenant_a.get_node(&node.id).await.expect("node").status,
        WorkNodeStatus::Draft
    );

    // Row-level security is the second layer: as the application role, tenant B's context
    // sees none of tenant A's rows, and no context sees nothing at all.
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("SET LOCAL ROLE quansio_app")
        .execute(&mut *tx)
        .await
        .expect("set role");
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT_B)
        .await
        .expect("tenant b context");
    let count: i64 = sqlx::query("SELECT count(*) AS n FROM work_nodes WHERE id = $1")
        .bind(node.id.to_string())
        .fetch_one(&mut *tx)
        .await
        .expect("count")
        .get("n");
    assert_eq!(count, 0, "RLS hides tenant A's rows from tenant B");

    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant a context");
    let count: i64 = sqlx::query("SELECT count(*) AS n FROM work_nodes WHERE id = $1")
        .bind(node.id.to_string())
        .fetch_one(&mut *tx)
        .await
        .expect("count")
        .get("n");
    assert_eq!(count, 1, "the owner tenant still sees its own row");
    tx.rollback().await.expect("rollback");

    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("SET LOCAL ROLE quansio_app")
        .execute(&mut *tx)
        .await
        .expect("set role");
    let visible: i64 = sqlx::query("SELECT count(*) AS n FROM work_nodes")
        .fetch_one(&mut *tx)
        .await
        .expect("count")
        .get("n");
    assert_eq!(visible, 0, "no tenant context fails closed");
    tx.rollback().await.expect("rollback");

    drop_pool(&pool, &name).await;
}
