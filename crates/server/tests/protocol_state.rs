//! Durable protocol state and checkpoint recovery tests (CORE-006).
//!
//! Recovery must reconstruct the next safe action from durable state alone — never from
//! semantic memory (DOSSIER.md §8, D-007). These tests persist protocol state, drop the
//! connection (simulating a crash), reconnect and assert the decision, including the
//! rule that an effect with an unknown outcome is reconciled rather than retried.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::runtime::checkpoints::{
    CheckpointError, CheckpointKind, CheckpointStore, NewCheckpoint,
};
use quansio_server::runtime::protocol_state::{
    next_safe_action, BrowserControl, BrowserControlHolder, NextAction, PendingToolCall,
    ProtocolState, ProtocolStateStore, Wait, WaitKind,
};

mod common;
use common::{
    admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, scratch_url, seed_tenant,
};

/// Run one store operation in its own tenant-scoped, committed transaction.
///
/// The stores take a `&mut PgConnection` so a caller can compose several of them into one
/// unit of work; the helper supplies that unit of work for tests that need exactly one.
macro_rules! with_tenant {
    ($pool:expr, $tenant:expr, |$conn:ident| $body:expr) => {{
        let mut tx = $pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, $tenant)
            .await
            .expect("tenant context");
        let result = {
            let $conn: &mut sqlx::PgConnection = &mut tx;
            $body
        };
        match result {
            Ok(value) => {
                tx.commit().await.expect("commit");
                Ok(value)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }};
}

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";

struct Fixture {
    name: String,
    url: String,
    pool: PgPool,
    run_id: String,
    agent_thread_id: String,
    target_id: String,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;

    let mut generator = UlidGenerator::new();
    let agent_thread_id = CanonicalId::generate(Prefix::AgentThread, &mut generator).to_string();
    let run_id = CanonicalId::generate(Prefix::Run, &mut generator).to_string();
    let target_id = CanonicalId::generate(Prefix::ExecutionTarget, &mut generator).to_string();

    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(&agent_thread_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    sqlx::query(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, \
         status, trigger_kind) VALUES ($1, $2, $3, $4, $5, 1, 'RUNNING', 'manual')",
    )
    .bind(&run_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(WORK_NODE)
    .bind(&agent_thread_id)
    .execute(&mut *tx)
    .await
    .expect("run");
    sqlx::query(
        "INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, status) \
         VALUES ($1, $2, $3, 'persistent_workspace_computer', 'local_capsule_macos', 'READY')",
    )
    .bind(&target_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("target");
    tx.commit().await.expect("commit");

    Some(Fixture {
        url: scratch_url(&name),
        name,
        pool,
        run_id,
        agent_thread_id,
        target_id,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn crash_restart_reconstructs_the_next_safe_action() {
    let Some(fixture) = prepare("protocol_resume").await else {
        blocked_marker();
        return;
    };

    let mut state = ProtocolState::new(&fixture.run_id, 1);
    state
        .pending_approvals
        .push("apr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string());
    state.waits.push(Wait {
        kind: WaitKind::Timer,
        key: "timer:daily-digest".to_string(),
        expires_at: Some("2026-09-13T00:00:00Z".to_string()),
    });
    with_tenant!(fixture.pool, TENANT, |conn| {
        ProtocolStateStore::store(conn, TENANT, &state).await
    })
    .expect("store state");

    // Simulate a process crash: drop the pool and reconnect to the same database.
    fixture.pool.close().await;
    let pool = PgPool::connect(&fixture.url)
        .await
        .expect("reconnect after crash");

    let mut conn = pool.acquire().await.expect("acquire");
    let resumed = ProtocolStateStore::resume(&mut conn, TENANT, &fixture.run_id)
        .await
        .expect("resume");
    assert_eq!(
        resumed,
        NextAction::WaitApproval {
            approval_id: "apr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string()
        }
    );

    let loaded = ProtocolStateStore::load(&mut conn, TENANT, &fixture.run_id)
        .await
        .expect("load")
        .expect("state exists");
    assert_eq!(loaded, state, "durable state must round-trip exactly");
    drop(conn);

    drop_pool(&pool, &fixture.name).await;
}

#[tokio::test]
async fn partial_tool_call_recovery_reconciles_and_never_retries() {
    let Some(fixture) = prepare("protocol_partial").await else {
        blocked_marker();
        return;
    };

    let mut state = ProtocolState::new(&fixture.run_id, 2);
    state.pending_tool_calls.push(PendingToolCall {
        tool_call_id: "tc_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string(),
        tool_name: "connector.github.create_issue".to_string(),
        dispatch_token: "dispatch-1".to_string(),
        effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string(),
        effect_status: "OUTCOME_UNKNOWN".to_string(),
    });
    with_tenant!(fixture.pool, TENANT, |conn| {
        ProtocolStateStore::store(conn, TENANT, &state).await
    })
    .expect("store state");

    let action = with_tenant!(fixture.pool, TENANT, |conn| {
        ProtocolStateStore::resume(conn, TENANT, &fixture.run_id).await
    })
    .expect("resume");
    match action {
        NextAction::ReconcileEffect {
            effect_id,
            tool_call_id,
        } => {
            assert_eq!(effect_id, "eff_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
            assert_eq!(tool_call_id, "tc_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
        }
        other => panic!("an unknown effect outcome must be reconciled, got {other:?}"),
    }

    // A settled effect does not block the loop.
    let mut settled = state.clone();
    settled.pending_tool_calls[0].effect_status = "SETTLED_SUCCESS".to_string();
    assert_eq!(next_safe_action(&settled), NextAction::Continue);

    finish(fixture).await;
}

#[tokio::test]
async fn approval_and_question_waits_survive_restart() {
    let Some(fixture) = prepare("protocol_waits").await else {
        blocked_marker();
        return;
    };

    let mut state = ProtocolState::new(&fixture.run_id, 3);
    state.pending_approvals.push("apr_x".to_string());
    state
        .open_questions
        .push("q_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string());
    state
        .child_agent_threads
        .push(fixture.agent_thread_id.clone());
    state.browser_control = Some(BrowserControl {
        session_id: "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string(),
        holder: BrowserControlHolder::User,
        since: Some("2026-09-12T01:00:00Z".to_string()),
    });

    with_tenant!(fixture.pool, TENANT, |conn| {
        ProtocolStateStore::store(conn, TENANT, &state).await
    })
    .expect("store");

    // Precedence: approval before question before browse takeover before child.
    assert!(matches!(
        next_safe_action(&state),
        NextAction::WaitApproval { .. }
    ));

    let mut without_approval = state.clone();
    without_approval.pending_approvals.clear();
    assert!(matches!(
        next_safe_action(&without_approval),
        NextAction::WaitQuestion { .. }
    ));

    let mut without_question = without_approval.clone();
    without_question.open_questions.clear();
    assert!(matches!(
        next_safe_action(&without_question),
        NextAction::AwaitHandback { .. }
    ));

    // Human takeover fences agent input until handback.
    let mut handed_back = without_question.clone();
    if let Some(control) = handed_back.browser_control.as_mut() {
        control.holder = BrowserControlHolder::Agent;
    }
    assert!(matches!(
        next_safe_action(&handed_back),
        NextAction::WaitChild { .. }
    ));

    // Cancellation always wins.
    let mut cancelled = handed_back.clone();
    cancelled.cancellation_requested = true;
    assert_eq!(next_safe_action(&cancelled), NextAction::Cancel);

    // Reloaded state still contains every pending item.
    let reloaded = with_tenant!(fixture.pool, TENANT, |conn| {
        ProtocolStateStore::load(conn, TENANT, &fixture.run_id).await
    })
    .expect("load")
    .expect("state");
    assert_eq!(reloaded.open_questions.len(), 1);
    assert_eq!(reloaded.child_agent_threads.len(), 1);
    assert!(reloaded.browser_control.is_some());

    finish(fixture).await;
}

#[tokio::test]
async fn protocol_state_is_tenant_scoped() {
    let Some(fixture) = prepare("protocol_tenant").await else {
        blocked_marker();
        return;
    };
    let state = ProtocolState::new(&fixture.run_id, 1);
    with_tenant!(fixture.pool, TENANT, |conn| {
        ProtocolStateStore::store(conn, TENANT, &state).await
    })
    .expect("store");

    // Another tenant sees nothing, even though the run id is known.
    let other = "tn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
    let mut tx = fixture.pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'other')")
        .bind(other)
        .execute(&mut *tx)
        .await
        .expect("other tenant");
    tx.commit().await.expect("commit");

    // RLS only constrains non-superusers, so the probe runs as the application role.
    let mut conn = fixture.pool.acquire().await.expect("acquire");
    sqlx::query("SET ROLE quansio_app")
        .execute(&mut *conn)
        .await
        .expect("set role");
    let hidden = ProtocolStateStore::load(&mut conn, other, &fixture.run_id)
        .await
        .expect("load as other tenant");
    assert!(
        hidden.is_none(),
        "protocol state must not leak across tenants"
    );
    sqlx::query("RESET ROLE")
        .execute(&mut *conn)
        .await
        .expect("reset role");
    drop(conn);

    finish(fixture).await;
}

#[tokio::test]
async fn checkpoints_are_generation_and_retention_fenced() {
    let Some(fixture) = prepare("checkpoint_fence").await else {
        blocked_marker();
        return;
    };

    let checkpoint_id = "ckp_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string();
    let stored = NewCheckpoint {
        id: checkpoint_id.clone(),
        execution_target_id: fixture.target_id.clone(),
        run_id: Some(fixture.run_id.clone()),
        kind: CheckpointKind::WorkspaceFiles,
        storage_ref: "s3://quansio/tenant/target/ckp.tar.zst#sha256:abc".to_string(),
        generation: 4,
        size_bytes: 1024,
        restore_policy: Some("auto_on_failure".to_string()),
        expires_at: Some("2026-09-20T00:00:00Z".to_string()),
    };
    with_tenant!(fixture.pool, TENANT, |conn| {
        CheckpointStore::create(conn, TENANT, &stored).await
    })
    .expect("create checkpoint");

    let listed = with_tenant!(fixture.pool, TENANT, |conn| {
        CheckpointStore::list(conn, TENANT, &fixture.target_id).await
    })
    .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].kind, CheckpointKind::WorkspaceFiles);
    assert_eq!(listed[0].generation, 4);

    // A controller at the creating generation may restore.
    let restored = with_tenant!(fixture.pool, TENANT, |conn| {
        CheckpointStore::validate_restore(conn, TENANT, &checkpoint_id, 4, "2026-09-12T00:00:00Z")
            .await
    })
    .expect("restore at the same generation");
    assert_eq!(restored.id, checkpoint_id);

    // An older controller may not (it would be fenced).
    let error = with_tenant!(fixture.pool, TENANT, |conn| {
        CheckpointStore::validate_restore(conn, TENANT, &checkpoint_id, 3, "2026-09-12T00:00:00Z")
            .await
    })
    .expect_err("stale controller");
    assert!(matches!(error, CheckpointError::StaleGeneration { .. }));

    // Retention is enforced.
    let expired = with_tenant!(fixture.pool, TENANT, |conn| {
        CheckpointStore::validate_restore(conn, TENANT, &checkpoint_id, 4, "2026-10-01T00:00:00Z")
            .await
    })
    .expect_err("expired checkpoint");
    assert!(matches!(expired, CheckpointError::Expired(_)));

    // Unknown checkpoints fail closed.
    let missing = with_tenant!(fixture.pool, TENANT, |conn| {
        CheckpointStore::validate_restore(
            conn,
            TENANT,
            "ckp_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ",
            4,
            "2026-09-12T00:00:00Z",
        )
        .await
    })
    .expect_err("missing checkpoint");
    assert!(matches!(missing, CheckpointError::NotFound(_)));

    finish(fixture).await;
}
