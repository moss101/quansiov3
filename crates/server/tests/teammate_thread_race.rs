//! Two messages routed to one teammate at once must share one thread (APP-009 over RUN-002).
//!
//! A teammate's row is committed before its AgentThread exists, so a second message can find the teammate and ask for its thread
//! while the first is still provisioning it. Provisioning was "look, then create, then activate" with a bare activation, so
//!
//! * the caller that created the thread could find it already activated by the other and fail with
//!   `INTERNAL: illegal agent_thread transition: ACTIVE -> ACTIVE` — a person posting at the same time as someone else saw an
//!   internal error — and
//! * two callers that both saw no thread each created their own, so one teammate ended up with two threads and the messages
//!   split between them.
//!
//! Real PostgreSQL. Without `QUANSIO_TEST_POSTGRES_URL` the suite reports `BLOCKED_EXTERNAL` and does not run.

use std::sync::Arc;

use quansio_core::{CanonicalId, CorrelationId, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::control::teammates::store::{NewTeammate, TeammateAdmin, TeammateStore};
use quansio_server::runtime::agents::AgentThreadStatus;
use quansio_server::runtime::state_machine::RuntimeIdentity;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YRACE01";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YRACE01";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YRACE01";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YRACE01";

fn identity() -> RuntimeIdentity {
    RuntimeIdentity::system(
        TENANT,
        "teammate-race-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

/// A teammate that exists and has no thread: the state between its row being committed and its thread being provisioned.
async fn teammate_without_a_thread(
    pool: &sqlx::PgPool,
) -> quansio_server::control::teammates::store::Teammate {
    let store = TeammateStore::new(pool.clone());
    let admin = TeammateAdmin {
        tenant_id: TENANT.to_string(),
        actor: identity().actor.clone(),
        correlation_id: identity().correlation_id,
    };
    let (teammate, thread) = store
        .create(
            &admin,
            &identity(),
            WORKSPACE,
            NewTeammate {
                template_id: Some("general-assistant".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create");
    let mut tx = pool.begin().await.unwrap();
    schema::set_tenant_context(&mut tx, TENANT).await.unwrap();
    sqlx::query("DELETE FROM agent_threads WHERE id = $1")
        .bind(thread.id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    teammate
}

async fn threads_of(pool: &sqlx::PgPool, teammate: &str) -> Vec<(String, String)> {
    let mut tx = pool.begin().await.unwrap();
    schema::set_tenant_context(&mut tx, TENANT).await.unwrap();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, status FROM agent_threads WHERE tenant_id = $1 AND definition_id = $2 ORDER BY created_at, id")
            .bind(TENANT)
            .bind(teammate)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    rows
}

#[tokio::test]
async fn concurrent_messages_to_one_teammate_share_one_active_thread_and_none_fails() {
    let Some(name) = admin_url().map(|_| scratch_name("tm_race")) else {
        return blocked_marker();
    };
    let Some(pool) = fresh_database(&name).await else {
        return blocked_marker();
    };
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;

    // Many rounds, because the race is a window: each round is a fresh teammate with no thread and eight callers at once.
    for round in 0..12 {
        let teammate = Arc::new(teammate_without_a_thread(&pool).await);
        let mut callers = Vec::new();
        for _ in 0..8 {
            let (pool, teammate) = (pool.clone(), Arc::clone(&teammate));
            callers.push(tokio::spawn(async move {
                TeammateStore::new(pool)
                    .provision_thread(&identity(), &teammate)
                    .await
            }));
        }
        let mut ids = Vec::new();
        for caller in callers {
            match caller.await.expect("task") {
                Ok(thread) => {
                    assert_eq!(thread.status, AgentThreadStatus::Active, "round {round}: a caller was handed a thread that is not active");
                    ids.push(thread.id.to_string());
                }
                Err(error) => panic!("round {round}: a message failed because another was posted at the same time: {error}"),
            }
        }
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            1,
            "round {round}: the callers were handed {} different threads for one teammate",
            ids.len()
        );
        let stored = threads_of(&pool, &teammate.id.to_string()).await;
        assert_eq!(
            stored.len(),
            1,
            "round {round}: the teammate has {} threads: {stored:?}",
            stored.len()
        );
        assert_eq!(stored[0].1, "ACTIVE");
        let _ = CanonicalId::parse(&stored[0].0).expect("a canonical id");
    }
    drop_pool(&pool, &name).await;
}
