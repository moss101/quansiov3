//! INT-008 coverage: an epoch produced outside any run cannot be installed against a run that
//! does not exist, and the epoch status spellings are closed (DOMAIN.md §5.7).
//!
//! Real boundary: none. The DB test needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`;
//! absent, it prints the `BLOCKED_EXTERNAL` marker and returns.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::compaction::{
    CompactionError, CompactionFence, CompactionStore, EpochStatus, InstallOutcome, NewEpoch,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA0C1";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA0C1";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0C1";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0C1";
const THREAD: &str = "thr_01J8Z3K6F1N8VQ2X5W9Y0QA0C1";
const MISSING_RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0QA0C1";

#[test]
fn epoch_statuses_round_trip_and_an_unknown_spelling_is_not_a_status() {
    for status in [
        EpochStatus::Pending,
        EpochStatus::Installed,
        EpochStatus::RejectedStale,
    ] {
        assert_eq!(EpochStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(EpochStatus::Pending.as_str(), "pending");
    assert_eq!(EpochStatus::RejectedStale.as_str(), "rejected_stale");
    assert_eq!(EpochStatus::parse("superseded"), None);
}

#[tokio::test]
async fn an_epoch_outside_any_run_is_refused_against_a_run_that_does_not_exist() {
    let Some(name) = admin_url().map(|_| scratch_name("qa001_compact_unowned")) else {
        blocked_marker();
        return;
    };
    let Some(pool) = fresh_database(&name).await else {
        blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;

    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO threads (id, tenant_id, workspace_id, kind, title) \
         VALUES ($1, $2, $3, 'objective', 'compaction fixture')",
    )
    .bind(THREAD)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("thread");
    let epoch = NewEpoch {
        id: CanonicalId::generate(Prefix::CompactionEpoch, &mut UlidGenerator::new()).to_string(),
        thread_id: THREAD.to_string(),
        // Produced by a thread-level compaction, not inside a run.
        run_id: None,
        seq: 1,
        source_from_sequence: 1,
        source_to_sequence: 20,
        summary_artifact_id: None,
        token_estimate: 64,
        created_by_model_route_id: None,
    };
    CompactionStore::create(&mut tx, TENANT, &epoch)
        .await
        .expect("create");

    // The caller names a run that is not in this tenant (never created, or gone): its history is
    // not a lineage anyone holds, so the epoch is refused and the refusal recorded.
    let fence = CompactionFence {
        run_id: MISSING_RUN.to_string(),
        generation: 1,
        position: 20,
    };
    let outcome = CompactionStore::install(&mut tx, TENANT, &epoch.id, &fence)
        .await
        .expect("decided");
    let InstallOutcome::Rejected {
        epoch: refused,
        refusal,
    } = outcome
    else {
        panic!("an epoch cannot be installed for a missing run: {outcome:?}");
    };
    assert_eq!(refused.status, EpochStatus::RejectedStale);
    match refusal {
        CompactionError::StaleRun {
            epoch_id,
            epoch_run,
            fence_run,
        } => {
            assert_eq!(epoch_id, epoch.id);
            assert_eq!(epoch_run, "", "the epoch belongs to no run");
            assert_eq!(fence_run, MISSING_RUN);
        }
        other => panic!("unexpected refusal: {other:?}"),
    }
    tx.commit().await.expect("commit");

    let mut tx = pool.begin().await.expect("begin");
    let listed = CompactionStore::list(&mut tx, TENANT, THREAD)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1, "the refused epoch is retained");
    assert_eq!(listed[0].status, EpochStatus::RejectedStale);
    let again = CompactionStore::install(&mut tx, TENANT, &epoch.id, &fence)
        .await
        .expect_err("a decided epoch is not decided twice");
    assert!(
        matches!(
            again,
            CompactionError::NotPending {
                status: "rejected_stale",
                ..
            }
        ),
        "{again:?}"
    );
    tx.rollback().await.expect("rollback");
    drop_pool(&pool, &name).await;
}
