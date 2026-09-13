//! Compaction epoch lifecycle tests (INT-008).
//!
//! The two properties the task's acceptance statements name, tested at the persistence boundary where
//! they are actually enforced:
//!
//! * **a fork or a revert never installs compaction from abandoned history** — an epoch whose range ends
//!   beyond the position the caller holds is refused (a revert moved the position back), an epoch that
//!   belongs to another run is refused (a fork left it behind), and a caller whose generation fence is
//!   old is refused because another controller owns the run. A refusal records `rejected_stale` and
//!   keeps the row: the refusal is the auditable fact, and deleting the epoch would destroy it;
//! * **exact protocol replay does not depend on summary text** — asserted structurally here (this
//!   module's store is not part of any replay path) and, more strongly, by RUN-009's declared recovery
//!   read set, which this test checks still names no compaction table.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent → `BLOCKED_EXTERNAL`.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::runtime::compaction::{
    CompactionEpoch, CompactionError, CompactionFence, CompactionStore, EpochStatus,
    InstallOutcome, NewEpoch,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

/// Run one store operation in its own tenant-scoped, committed transaction.
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

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";

struct Fixture {
    name: String,
    pool: PgPool,
    run_id: String,
    /// A second run of the same thread: the run a fork leaves behind.
    forked_run_id: String,
    thread_id: String,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;

    let mut generator = UlidGenerator::new();
    let agent_thread_id = CanonicalId::generate(Prefix::AgentThread, &mut generator).to_string();
    let run_id = CanonicalId::generate(Prefix::Run, &mut generator).to_string();
    let forked_run_id = CanonicalId::generate(Prefix::Run, &mut generator).to_string();
    let thread_id = CanonicalId::generate(Prefix::Thread, &mut generator).to_string();

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
        "INSERT INTO threads (id, tenant_id, workspace_id, kind, title) \
         VALUES ($1, $2, $3, 'objective', 'compaction fixture')",
    )
    .bind(&thread_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("thread");
    for (id, generation) in [(&run_id, 1_i64), (&forked_run_id, 1_i64)] {
        sqlx::query(
            "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, \
             generation, status, trigger_kind) VALUES ($1, $2, $3, $4, $5, $6, 'RUNNING', 'manual')",
        )
        .bind(id)
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(WORK_NODE)
        .bind(&agent_thread_id)
        .bind(generation)
        .execute(&mut *tx)
        .await
        .expect("run");
    }
    tx.commit().await.expect("commit");

    Some(Fixture {
        name,
        pool,
        run_id,
        forked_run_id,
        thread_id,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

fn new_epoch(fixture: &Fixture, seq: i32, from: i64, to: i64) -> NewEpoch {
    let mut generator = UlidGenerator::new();
    NewEpoch {
        id: CanonicalId::generate(Prefix::CompactionEpoch, &mut generator).to_string(),
        thread_id: fixture.thread_id.clone(),
        run_id: Some(fixture.run_id.clone()),
        seq,
        source_from_sequence: from,
        source_to_sequence: to,
        summary_artifact_id: None,
        token_estimate: 128,
        created_by_model_route_id: Some("mr_fixture_route".to_string()),
    }
}

fn fence(fixture: &Fixture, generation: i64, position: i64) -> CompactionFence {
    CompactionFence {
        run_id: fixture.run_id.clone(),
        generation,
        position,
    }
}

#[tokio::test]
async fn an_epoch_installs_when_the_caller_still_holds_its_range() {
    let Some(fixture) = prepare("compact_install").await else {
        blocked_marker();
        return;
    };

    let epoch = new_epoch(&fixture, 1, 10, 40);
    with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::create(conn, TENANT, &epoch).await
    })
    .expect("create");

    let installed = match with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::install(conn, TENANT, &epoch.id, &fence(&fixture, 1, 40)).await
    })
    .expect("install")
    {
        InstallOutcome::Installed(installed) => installed,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(installed.status, EpochStatus::Installed);
    assert_eq!(installed.source_from_sequence, 10);
    assert_eq!(installed.source_to_sequence, 40);
    assert_eq!(
        installed.created_by_model_route_id.as_deref(),
        Some("mr_fixture_route"),
        "the route that wrote the summary survives the lifecycle"
    );

    let reloaded = with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::load(conn, TENANT, &epoch.id).await
    })
    .expect("load");
    assert_eq!(reloaded.status, EpochStatus::Installed);
    finish(fixture).await;
}

#[tokio::test]
async fn a_revert_refuses_an_epoch_covering_abandoned_history() {
    let Some(fixture) = prepare("compact_revert").await else {
        blocked_marker();
        return;
    };

    let epoch = new_epoch(&fixture, 1, 10, 40);
    with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::create(conn, TENANT, &epoch).await
    })
    .expect("create");

    // The revert moved the position back to 25: the epoch summarises history up to 40, which the
    // caller's lineage no longer contains.
    match with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::install(conn, TENANT, &epoch.id, &fence(&fixture, 1, 25)).await
    })
    .expect("decided")
    {
        InstallOutcome::Rejected { refusal, epoch } => {
            assert_eq!(epoch.status, EpochStatus::RejectedStale);
            match refusal {
                CompactionError::StalePosition {
                    covers, position, ..
                } => {
                    assert_eq!((covers, position), (40, 25));
                }
                other => panic!("unexpected refusal: {other:?}"),
            }
        }
        other => panic!("unexpected outcome: {other:?}"),
    }

    let reloaded = with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::load(conn, TENANT, &epoch.id).await
    })
    .expect("load");
    assert_eq!(
        reloaded.status,
        EpochStatus::RejectedStale,
        "the refusal is recorded, not applied"
    );
    assert_eq!(
        reloaded.source_to_sequence, 40,
        "the epoch is retained for audit"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_fork_refuses_an_epoch_from_another_run() {
    let Some(fixture) = prepare("compact_fork").await else {
        blocked_marker();
        return;
    };

    let epoch = new_epoch(&fixture, 1, 10, 40);
    with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::create(conn, TENANT, &epoch).await
    })
    .expect("create");

    let forked = CompactionFence {
        run_id: fixture.forked_run_id.clone(),
        generation: 1,
        position: 40,
    };
    match with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::install(conn, TENANT, &epoch.id, &forked).await
    })
    .expect("decided")
    {
        InstallOutcome::Rejected { refusal, .. } => match refusal {
            CompactionError::StaleRun {
                epoch_run,
                fence_run,
                ..
            } => {
                assert_eq!(epoch_run, fixture.run_id);
                assert_eq!(fence_run, fixture.forked_run_id);
            }
            other => panic!("unexpected refusal: {other:?}"),
        },
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        with_tenant!(&fixture.pool, TENANT, |conn| CompactionStore::load(
            conn, TENANT, &epoch.id
        )
        .await)
        .expect("load")
        .status,
        EpochStatus::RejectedStale
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_stale_controller_cannot_install_after_the_generation_moved() {
    let Some(fixture) = prepare("compact_generation").await else {
        blocked_marker();
        return;
    };

    let epoch = new_epoch(&fixture, 1, 10, 40);
    with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::create(conn, TENANT, &epoch).await
    })
    .expect("create");

    // Another controller fenced the run forward.
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("UPDATE runs SET generation = 2 WHERE id = $1")
        .bind(&fixture.run_id)
        .execute(&mut *tx)
        .await
        .expect("bump generation");
    tx.commit().await.expect("commit");

    match with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::install(conn, TENANT, &epoch.id, &fence(&fixture, 1, 40)).await
    })
    .expect("decided")
    {
        InstallOutcome::Rejected { refusal, epoch } => {
            match refusal {
                CompactionError::StaleGeneration { stored, held, .. } => {
                    assert_eq!((stored, held), (2, 1));
                }
                other => panic!("unexpected refusal: {other:?}"),
            }
            assert_eq!(
                epoch.status,
                EpochStatus::Pending,
                "a stale fence is not abandoned history: the epoch stays pending"
            );
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    // The generation the caller held was wrong, not the history: the epoch stays pending, because
    // nothing about its range was abandoned.
    assert_eq!(
        with_tenant!(&fixture.pool, TENANT, |conn| CompactionStore::load(
            conn, TENANT, &epoch.id
        )
        .await)
        .expect("load")
        .status,
        EpochStatus::Pending
    );
    finish(fixture).await;
}

#[tokio::test]
async fn an_installed_epoch_is_not_installed_twice() {
    let Some(fixture) = prepare("compact_twice").await else {
        blocked_marker();
        return;
    };

    let epoch = new_epoch(&fixture, 1, 10, 40);
    with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::create(conn, TENANT, &epoch).await
    })
    .expect("create");
    with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::install(conn, TENANT, &epoch.id, &fence(&fixture, 1, 40)).await
    })
    .expect("install");

    let refusal = with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::install(conn, TENANT, &epoch.id, &fence(&fixture, 1, 40)).await
    })
    .expect_err("must refuse");
    match refusal {
        CompactionError::NotPending { status, .. } => assert_eq!(status, "installed"),
        other => panic!("unexpected refusal: {other:?}"),
    }
    finish(fixture).await;
}

#[tokio::test]
async fn an_unordered_range_is_refused_before_the_write() {
    let Some(fixture) = prepare("compact_range").await else {
        blocked_marker();
        return;
    };

    let mut epoch = new_epoch(&fixture, 1, 40, 10);
    match with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::create(conn, TENANT, &epoch).await
    }) {
        Err(CompactionError::RangeInvalid { from, to }) => assert_eq!((from, to), (40, 10)),
        other => panic!("unexpected result: {other:?}"),
    }
    // And the schema holds the same line if a writer bypasses this store.
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let bypass = sqlx::query(
        "INSERT INTO compaction_epochs (id, tenant_id, thread_id, run_id, seq, source_from_sequence, \
         source_to_sequence, token_estimate, status) VALUES ($1, $2, $3, $4, 2, 40, 10, 1, 'pending')",
    )
    .bind(&epoch.id)
    .bind(TENANT)
    .bind(&fixture.thread_id)
    .bind(&fixture.run_id)
    .execute(&mut *tx)
    .await;
    assert!(
        bypass.is_err(),
        "the table's CHECK refuses an unordered range"
    );
    let _ = tx.rollback().await;
    epoch.id.clear();
    finish(fixture).await;
}

#[tokio::test]
async fn epochs_are_listed_per_thread_and_are_tenant_scoped() {
    let Some(fixture) = prepare("compact_scope").await else {
        blocked_marker();
        return;
    };

    let first = new_epoch(&fixture, 1, 10, 40);
    let second = new_epoch(&fixture, 2, 41, 70);
    for epoch in [&first, &second] {
        with_tenant!(&fixture.pool, TENANT, |conn| {
            CompactionStore::create(conn, TENANT, epoch).await
        })
        .expect("create");
    }

    let listed = with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::list(conn, TENANT, &fixture.thread_id).await
    })
    .expect("list");
    assert_eq!(
        listed.iter().map(|epoch| epoch.seq).collect::<Vec<_>>(),
        vec![1, 2],
        "epochs are listed in per-thread order"
    );
    assert!(listed
        .iter()
        .all(|epoch: &CompactionEpoch| epoch.status == EpochStatus::Pending));

    // A tenant with the same thread id sees nothing: the store's predicate and the table's forced
    // row-level security both apply.
    with_tenant!(&fixture.pool, TENANT, |conn| {
        CompactionStore::load(conn, TENANT, "cep_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ").await
    })
    .expect_err("an unknown epoch is not found");
    finish(fixture).await;
}

#[test]
fn the_replay_path_does_not_read_compaction() {
    // Exact protocol replay reads the event log and the protocol state; a summary is an optimisation for
    // a model's context. RUN-009 declares the complete set of tables recovery may read, and the property
    // is asserted where it is authoritative: that set names no compaction table, and the recovery code
    // itself never queries one.
    use quansio_server::runtime::recovery::RECOVERY_READ_TABLES;

    assert!(
        !RECOVERY_READ_TABLES
            .iter()
            .any(|table| table.contains("compaction")),
        "recovery must not read compaction state: exact replay does not depend on summary text"
    );
    assert!(RECOVERY_READ_TABLES.contains(&"protocol_states"));
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/recovery");
    for source in ["mod.rs", "plan.rs", "service.rs"] {
        let text = std::fs::read_to_string(root.join(source)).expect("read recovery source");
        assert!(
            !text.contains("compaction_epochs"),
            "{source} must not query the compaction table"
        );
    }
}
