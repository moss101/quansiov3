//! Duplicate-command and stale-generation tests (CORE-002) against real PostgreSQL.
//!
//! Replay safety has to hold at the database, not only in memory: the schema owns the
//! uniqueness constraint for `command_id` and the generation guard for mutations, while
//! `quansio-core` owns the typed classification. These tests drive both together.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN for the scratch database);
//! see `tests/common/mod.rs`. Absent → `BLOCKED_EXTERNAL` marker.

use quansio_core::{
    classify_duplicate, CanonicalId, CommandId, CoreError, Digest, DuplicateOutcome, Generation,
    Prefix, TypedId, UlidGenerator,
};
use sqlx::PgPool;

use quansio_server::control::schema;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    Some((name, pool))
}

#[tokio::test]
async fn duplicate_commands_replay_and_param_changes_conflict() {
    let Some((name, pool)) = prepare("duplicate").await else {
        blocked_marker();
        return;
    };

    let mut generator = UlidGenerator::new();
    let command: CommandId = CommandId::generate(&mut generator);
    let params = Digest::of_canonical_json(r#"{"objective":"ship the release"}"#);

    // First submission records the command and its parameter digest.
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let inserted = sqlx::query(
        "INSERT INTO commands (id, tenant_id, workspace_id, command_name, command_id, params_digest, \
         status, result) VALUES ($1, $2, $3, 'CreateObjective', $4, $5, 'applied', \
         '{\"objective_id\":\"wn_x\"}'::jsonb)",
    )
    .bind(format!("cmd_{}", command.as_id().ulid()))
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(command.to_string())
    .bind(params.as_str())
    .execute(&mut *tx)
    .await
    .expect("first submission");
    assert_eq!(inserted.rows_affected(), 1);
    tx.commit().await.expect("commit");

    // A duplicate submission with the same parameters is a replay: the database refuses
    // a second row, and the classification says "return the recorded result".
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let duplicate = sqlx::query(
        "INSERT INTO commands (id, tenant_id, workspace_id, command_name, command_id, params_digest, \
         status, result) VALUES ($1, $2, $3, 'CreateObjective', $4, $5, 'applied', '{}'::jsonb)",
    )
    .bind(format!("cmd_{}", command.as_id().ulid()))
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(command.to_string())
    .bind(params.as_str())
    .execute(&mut *tx)
    .await;
    assert!(
        duplicate.is_err(),
        "the database must reject a duplicate (tenant, command_id)"
    );
    tx.rollback().await.expect("rollback");

    assert_eq!(
        classify_duplicate(&command, &params, &params).expect("replay"),
        DuplicateOutcome::Replay
    );

    // The same command id with different parameters is a conflict, not a silent second
    // mutation (DOMAIN.md §15 CONFLICT_IDEMPOTENCY_MISMATCH).
    let changed = Digest::of_canonical_json(r#"{"objective":"ship something else"}"#);
    let error = classify_duplicate(&command, &params, &changed).expect_err("conflict");
    assert_eq!(
        error,
        CoreError::IdempotencyMismatch {
            command_id: command.to_string()
        }
    );

    let stored: (i64, String) = sqlx::query_as(
        "SELECT count(*), min(result::text) FROM commands WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(TENANT)
    .bind(command.to_string())
    .fetch_one(&pool)
    .await
    .expect("stored command");
    assert_eq!(stored.0, 1, "exactly one command row");
    assert!(
        stored.1.contains("objective_id"),
        "the recorded result is preserved for the replay"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn stale_generations_are_rejected_before_mutation() {
    let Some((name, pool)) = prepare("generation").await else {
        blocked_marker();
        return;
    };

    let mut generator = UlidGenerator::new();
    let agent_thread: CanonicalId = CanonicalId::generate(Prefix::AgentThread, &mut generator);
    let run: CanonicalId = CanonicalId::generate(Prefix::Run, &mut generator);
    let current = Generation::new(5).expect("valid generation");

    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', $4, 'ACTIVE')",
    )
    .bind(agent_thread.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(current.get() as i64)
    .execute(&mut *tx)
    .await
    .expect("insert agent thread");
    sqlx::query(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, \
         status, trigger_kind) VALUES ($1, $2, $3, $4, $5, $6, 'QUEUED', 'manual')",
    )
    .bind(run.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(WORK_NODE)
    .bind(agent_thread.to_string())
    .bind(current.get() as i64)
    .execute(&mut *tx)
    .await
    .expect("insert run");
    tx.commit().await.expect("commit");

    // The typed guard rejects a stale sender before any statement is issued.
    let stale = Generation::new(current.get() - 1).expect("valid");
    assert!(matches!(
        current.accept(stale),
        Err(CoreError::StaleGeneration { .. })
    ));
    assert!(current.accept(current).is_ok());
    assert!(current.accept(current.next()).is_ok());

    // The database guard is compare-and-set: a stale generation updates zero rows.
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let stale_update = sqlx::query(
        "UPDATE runs SET status = 'RUNNING', generation = generation + 1 \
         WHERE id = $1 AND generation = $2",
    )
    .bind(run.to_string())
    .bind(stale.get() as i64)
    .execute(&mut *tx)
    .await
    .expect("stale update executes");
    assert_eq!(
        stale_update.rows_affected(),
        0,
        "a stale generation must not mutate state"
    );

    let fresh_update = sqlx::query(
        "UPDATE runs SET status = 'RUNNING', generation = generation + 1 \
         WHERE id = $1 AND generation = $2",
    )
    .bind(run.to_string())
    .bind(current.get() as i64)
    .execute(&mut *tx)
    .await
    .expect("fresh update executes");
    assert_eq!(fresh_update.rows_affected(), 1);
    tx.commit().await.expect("commit");

    let (status, generation): (String, i64) =
        sqlx::query_as("SELECT status, generation FROM runs WHERE id = $1")
            .bind(run.to_string())
            .fetch_one(&pool)
            .await
            .expect("run row");
    assert_eq!(status, "RUNNING");
    assert_eq!(generation, current.next().get() as i64);

    // A stale worker message is fenced out by the (lease_id, generation) token.
    let lease: CanonicalId = CanonicalId::generate(Prefix::Lease, &mut generator);
    let token = quansio_core::FenceToken::new(lease, current.next());
    assert!(token.accept(&lease, current).is_err());
    assert!(token.accept(&lease, current.next()).is_ok());

    // Cleanup helper table check: the run remained visible to its tenant only.
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("SET LOCAL ROLE quansio_app")
        .execute(&mut *tx)
        .await
        .expect("set role");
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM runs")
        .fetch_one(&mut *tx)
        .await
        .expect("count without context");
    assert_eq!(visible, 0, "runs are invisible without tenant context");
    tx.rollback().await.expect("rollback");

    drop_pool(&pool, &name).await;
}
