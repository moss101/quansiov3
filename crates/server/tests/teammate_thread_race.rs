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
//! The same window exists one level up: a workspace's first messages can all find no teammate, and each created its own default
//! teammate. And a teammate given several threads by the old defect keeps the first; the others are retired once idle.
//!
//! Real PostgreSQL. Without `QUANSIO_TEST_POSTGRES_URL` the suite reports `BLOCKED_EXTERNAL` and does not run.

use std::sync::Arc;

use quansio_core::{CanonicalId, CorrelationId, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::control::teammates::store::{NewTeammate, TeammateAdmin, TeammateStore};
use quansio_server::runtime::agents::{AgentKind, AgentStore, AgentThreadStatus, NewAgentThread};
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

fn admin() -> TeammateAdmin {
    TeammateAdmin {
        tenant_id: TENANT.to_string(),
        actor: identity().actor.clone(),
        correlation_id: identity().correlation_id,
    }
}

async fn sql(pool: &sqlx::PgPool, statement: &str, binds: &[&str]) {
    let mut tx = pool.begin().await.unwrap();
    schema::set_tenant_context(&mut tx, TENANT).await.unwrap();
    let mut query = sqlx::query(statement);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

async fn default_teammates(pool: &sqlx::PgPool) -> Vec<String> {
    let mut tx = pool.begin().await.unwrap();
    schema::set_tenant_context(&mut tx, TENANT).await.unwrap();
    sqlx::query_scalar(
        "SELECT id FROM teammates WHERE tenant_id = $1 AND workspace_id = $2 AND status = 'active' \
         AND availability = 'available' ORDER BY created_at, id",
    )
    .bind(TENANT)
    .bind(WORKSPACE)
    .fetch_all(&mut *tx)
    .await
    .unwrap()
}

#[tokio::test]
async fn concurrent_first_messages_to_a_workspace_create_one_default_teammate() {
    let Some(name) = admin_url().map(|_| scratch_name("tm_default")) else {
        return blocked_marker();
    };
    let Some(pool) = fresh_database(&name).await else {
        return blocked_marker();
    };
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;

    for round in 0..12 {
        // A workspace with no teammate a message could go to: the previous round's are archived.
        sql(
            &pool,
            "UPDATE teammates SET status = 'archived' WHERE tenant_id = $1 AND workspace_id = $2",
            &[TENANT, WORKSPACE],
        )
        .await;
        let mut callers = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            callers.push(tokio::spawn(async move {
                TeammateStore::new(pool)
                    .ensure_default(&admin(), &identity(), WORKSPACE)
                    .await
            }));
        }
        let mut teammates = Vec::new();
        let mut threads = Vec::new();
        for caller in callers {
            match caller.await.expect("task") {
                Ok((teammate, thread)) => {
                    assert_eq!(teammate.name, "General assistant");
                    assert_eq!(thread.status, AgentThreadStatus::Active, "round {round}");
                    teammates.push(teammate.id.to_string());
                    threads.push(thread.id.to_string());
                }
                Err(error) => panic!("round {round}: a first message failed because another was posted at the same time: {error}"),
            }
        }
        teammates.sort();
        teammates.dedup();
        threads.sort();
        threads.dedup();
        assert_eq!(
            teammates.len(),
            1,
            "round {round}: the callers were handed {} default teammates",
            teammates.len()
        );
        assert_eq!(
            threads.len(),
            1,
            "round {round}: the callers were handed {} threads",
            threads.len()
        );
        let stored = default_teammates(&pool).await;
        assert_eq!(
            stored,
            teammates,
            "round {round}: the workspace has {} default teammates",
            stored.len()
        );
        assert_eq!(
            threads_of(&pool, &stored[0]).await.len(),
            1,
            "round {round}"
        );
    }
    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_teammates_duplicate_threads_are_retired_once_idle_and_the_first_is_kept() {
    let Some(name) = admin_url().map(|_| scratch_name("tm_dupes")) else {
        return blocked_marker();
    };
    let Some(pool) = fresh_database(&name).await else {
        return blocked_marker();
    };
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;

    let store = TeammateStore::new(pool.clone());
    let (teammate, first) = store
        .ensure_default(&admin(), &identity(), WORKSPACE)
        .await
        .expect("default");
    // What the old defect left behind: two more threads for the same teammate, one of them still answering a conversation.
    let agents = AgentStore::new(pool.clone(), identity()).expect("agents");
    let mut extra = Vec::new();
    for _ in 0..2 {
        let mut new = NewAgentThread::new(WORKSPACE.to_string(), AgentKind::Teammate);
        new.definition_id = Some(teammate.id);
        let thread = agents.create_thread(new).await.expect("duplicate");
        extra.push(
            agents
                .ensure_active(thread)
                .await
                .expect("active")
                .id
                .to_string(),
        );
    }
    let (idle, busy) = (extra[0].clone(), extra[1].clone());
    sql(
        &pool,
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, status, trigger_kind) \
         VALUES ('run_01J8Z3K6F1N8VQ2X5W9YRACE01', $1, $2, $3, $4, 'RUNNING', 'message')",
        &[TENANT, WORKSPACE, NODE, &busy],
    )
    .await;

    let kept = store
        .provision_thread(&identity(), &teammate)
        .await
        .expect("provision");
    assert_eq!(kept.id, first.id, "the first thread is the teammate's");
    let status = |rows: &[(String, String)], id: &str| {
        rows.iter()
            .find(|(row, _)| row == id)
            .map(|(_, status)| status.clone())
            .expect("thread")
    };
    let rows = threads_of(&pool, &teammate.id.to_string()).await;
    assert_eq!(status(&rows, &first.id.to_string()), "ACTIVE");
    assert_eq!(
        status(&rows, &idle),
        "TERMINATED",
        "an idle duplicate is retired"
    );
    assert_eq!(
        status(&rows, &busy),
        "ACTIVE",
        "a duplicate whose run is still open is left to finish it"
    );

    sql(
        &pool,
        "UPDATE runs SET status = 'SUCCEEDED' WHERE id = 'run_01J8Z3K6F1N8VQ2X5W9YRACE01'",
        &[],
    )
    .await;
    store
        .provision_thread(&identity(), &teammate)
        .await
        .expect("provision");
    let rows = threads_of(&pool, &teammate.id.to_string()).await;
    assert_eq!(
        status(&rows, &busy),
        "TERMINATED",
        "retired once its run ended"
    );
    assert_eq!(status(&rows, &first.id.to_string()), "ACTIVE");
    drop_pool(&pool, &name).await;
}
