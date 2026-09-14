//! Browser sessions against real PostgreSQL (EXEC-009).
//!
//! The worker's half of a session is tested in `crates/qworkerd/tests/browser.rs`, against a real Chrome.
//! This suite proves the half that has to survive the worker: the row a takeover and a resume read, and
//! the rules §8.4 states, enforced at the durable boundary so two callers cannot disagree about who holds
//! the browser.
//!
//! When the environment provides no database the suite reports `BLOCKED_EXTERNAL` and returns, so a
//! missing dev stack is never a pass.

mod common;

use quansio_machine::control::browser::{
    BrowserSessionError, BrowserSessionStore, ControlHolder, SessionStatus,
};
use quansio_machine::control::{MachineControl, NewTarget, Substrate, TargetClass};
use sqlx::{PgConnection, PgPool};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const TARGET_ID: &str = "CDP-TARGET-1";
const AT: &str = "2026-09-14T10:00:00Z";

async fn seed(pool: &PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    common::seed_tenant(pool, TENANT, USER, WORKSPACE).await;
    common::seed_tenant(pool, OTHER_TENANT, OTHER_USER, OTHER_WORKSPACE).await;
    let mut pooled = pool.acquire().await.expect("acquire");
    let conn: &mut PgConnection = &mut pooled;
    MachineControl::register(
        conn,
        TENANT,
        &NewTarget {
            id: TARGET.to_string(),
            workspace_id: WORKSPACE.to_string(),
            class: TargetClass::IsolatedTaskRuntime,
            substrate: Substrate::CloudMicrovm,
            image_digest: None,
            desired_state: None,
        },
    )
    .await
    .expect("register target");
    pooled
}

async fn open(conn: &mut PgConnection) -> quansio_machine::control::browser::BrowserSession {
    BrowserSessionStore::open(conn, TENANT, SESSION, TARGET, None, Some("profile-1"))
        .await
        .expect("open session")
}

#[tokio::test]
async fn a_session_survives_its_worker_and_a_takeover_hands_over_the_same_row() {
    let name = common::scratch_name("browser_session");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    let session = open(conn).await;
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.control_holder, ControlHolder::Agent);
    assert!(session.agent_may_drive());
    assert_eq!(session.profile_ref.as_deref(), Some("profile-1"));
    assert!(session.tabs.is_empty());

    // The page is recorded, and the tab list follows it.
    let recorded = BrowserSessionStore::record_page(
        conn,
        TENANT,
        SESSION,
        TARGET_ID,
        "https://example.com/a",
        "A",
    )
    .await
    .expect("record");
    assert_eq!(
        recorded.current_url.as_deref(),
        Some("https://example.com/a")
    );
    assert_eq!(recorded.tabs.len(), 1);
    assert_eq!(recorded.tabs[0].title, "A");
    // The same target moving to a new page updates the tab rather than adding one.
    let moved = BrowserSessionStore::record_page(
        conn,
        TENANT,
        SESSION,
        TARGET_ID,
        "https://example.com/b",
        "B",
    )
    .await
    .expect("record");
    assert_eq!(
        moved.tabs.len(),
        1,
        "the tab list grew instead of following the page"
    );
    assert_eq!(moved.tabs[0].url, "https://example.com/b");
    // A second tab is a second entry.
    let second = BrowserSessionStore::record_page(
        conn,
        TENANT,
        SESSION,
        "CDP-TARGET-2",
        "https://example.com/c",
        "C",
    )
    .await
    .expect("record");
    assert_eq!(second.tabs.len(), 2);

    // A *different* connection sees all of it: the state is durable, not the caller's.
    let mut other = pool.acquire().await.expect("acquire");
    let reloaded = BrowserSessionStore::load(&mut other, TENANT, SESSION)
        .await
        .expect("reload");
    assert_eq!(
        reloaded.current_url.as_deref(),
        Some("https://example.com/c")
    );
    assert_eq!(reloaded.tabs.len(), 2);

    // The user takes over. Input is fenced and the session is *paused*, not duplicated: the tab, the
    // profile and the page are all still here, which is what §8.4 requires.
    let taken = BrowserSessionStore::request_takeover(&mut other, TENANT, SESSION, AT)
        .await
        .expect("takeover");
    assert_eq!(taken.control_holder, ControlHolder::User);
    assert_eq!(taken.status, SessionStatus::PausedTakeover);
    assert_eq!(taken.control_since.as_deref(), Some(AT));
    assert!(!taken.agent_may_drive());
    assert_eq!(taken.tabs.len(), 2, "a takeover duplicated the session");
    assert_eq!(taken.profile_ref.as_deref(), Some("profile-1"));
    // A second takeover is refused rather than queued.
    assert!(matches!(
        BrowserSessionStore::request_takeover(&mut other, TENANT, SESSION, AT).await,
        Err(BrowserSessionError::NotAllowed {
            status: "paused_takeover",
            ..
        })
    ));

    // Handing back returns control to the agent, on the same page.
    let back = BrowserSessionStore::handback(&mut other, TENANT, SESSION, "2026-09-14T10:01:00Z")
        .await
        .expect("handback");
    assert_eq!(back.control_holder, ControlHolder::Agent);
    assert_eq!(back.status, SessionStatus::Active);
    assert!(back.agent_may_drive());
    assert_eq!(back.current_url.as_deref(), Some("https://example.com/c"));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_policy_pause_cannot_be_resumed_by_a_handback() {
    let name = common::scratch_name("browser_policy");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    open(conn).await;

    let paused = BrowserSessionStore::pause_for_policy(conn, TENANT, SESSION, AT)
        .await
        .expect("pause");
    assert_eq!(paused.status, SessionStatus::PausedPolicy);
    assert_eq!(paused.control_holder, ControlHolder::Nobody);
    assert!(!paused.agent_may_drive());

    // Nobody holds it, so there is nothing to hand back: a handback cannot undo a policy decision.
    assert!(matches!(
        BrowserSessionStore::handback(conn, TENANT, SESSION, AT).await,
        Err(BrowserSessionError::NotHeld { holder: "none", .. })
    ));
    assert_eq!(
        BrowserSessionStore::load(conn, TENANT, SESSION)
            .await
            .expect("load")
            .status,
        SessionStatus::PausedPolicy
    );
    // And it cannot be taken over either: it is not active.
    assert!(matches!(
        BrowserSessionStore::request_takeover(conn, TENANT, SESSION, AT).await,
        Err(BrowserSessionError::NotAllowed { .. })
    ));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_closed_session_is_terminal_and_keeps_its_checkpoint() {
    let name = common::scratch_name("browser_close");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    open(conn).await;
    BrowserSessionStore::record_page(
        conn,
        TENANT,
        SESSION,
        TARGET_ID,
        "https://example.com/",
        "Home",
    )
    .await
    .expect("record");

    // A checkpoint is a place a resume can come back to. It is a real row with a real foreign key, so the
    // fixture creates it rather than naming one that does not exist.
    let checkpoint = "ckp_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
    sqlx::query(
        "INSERT INTO checkpoints (id, tenant_id, execution_target_id, kind, storage_ref, generation) \
         VALUES ($1, $2, $3, 'browser_session', 's3://checkpoints/one', 1)",
    )
    .bind(checkpoint)
    .bind(TENANT)
    .bind(TARGET)
    .execute(&mut *conn)
    .await
    .expect("seed the checkpoint");
    let saved = BrowserSessionStore::attach_checkpoint(conn, TENANT, SESSION, checkpoint)
        .await
        .expect("checkpoint");
    assert_eq!(saved.checkpoint_id.as_deref(), Some(checkpoint));

    let closed = BrowserSessionStore::close(conn, TENANT, SESSION, AT)
        .await
        .expect("close");
    assert_eq!(closed.status, SessionStatus::Closed);
    assert_eq!(closed.control_holder, ControlHolder::Nobody);
    assert_eq!(closed.screencast, serde_json::json!({}));
    // What the session was is still there to read afterwards.
    assert_eq!(closed.checkpoint_id.as_deref(), Some(checkpoint));
    assert_eq!(closed.current_url.as_deref(), Some("https://example.com/"));

    // A closed session is terminal: no takeover, and the refusal names the state.
    assert!(matches!(
        BrowserSessionStore::request_takeover(conn, TENANT, SESSION, AT).await,
        Err(BrowserSessionError::NotAllowed {
            status: "closed",
            ..
        })
    ));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_session_is_scoped_to_its_tenant_and_checked_against_its_target() {
    let name = common::scratch_name("browser_scope");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // A target the tenant does not have is refused rather than leaving a dangling session.
    assert!(matches!(
        BrowserSessionStore::open(
            conn,
            TENANT,
            SESSION,
            "tgt_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ",
            None,
            None
        )
        .await,
        Err(BrowserSessionError::TargetNotFound(_))
    ));
    // An id that is not a `bsn_` id is refused before anything is written.
    assert!(matches!(
        BrowserSessionStore::open(
            conn,
            TENANT,
            "brow_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
            TARGET,
            None,
            None
        )
        .await,
        Err(BrowserSessionError::SessionIdInvalid(_))
    ));
    assert!(BrowserSessionStore::list_for_target(conn, TENANT, TARGET)
        .await
        .expect("list")
        .is_empty());

    open(conn).await;
    // Another tenant can neither see nor change it, and its own listing stays empty.
    assert!(matches!(
        BrowserSessionStore::load(conn, OTHER_TENANT, SESSION).await,
        Err(BrowserSessionError::NotFound(_))
    ));
    assert!(matches!(
        BrowserSessionStore::request_takeover(conn, OTHER_TENANT, SESSION, AT).await,
        Err(BrowserSessionError::NotFound(_))
    ));
    assert!(matches!(
        BrowserSessionStore::record_page(conn, OTHER_TENANT, SESSION, TARGET_ID, "https://x/", "x")
            .await,
        Err(BrowserSessionError::NotFound(_))
    ));
    assert!(matches!(
        BrowserSessionStore::close(conn, OTHER_TENANT, SESSION, AT).await,
        Err(BrowserSessionError::NotFound(_))
    ));
    assert!(
        BrowserSessionStore::list_for_target(conn, OTHER_TENANT, TARGET)
            .await
            .expect("list")
            .is_empty()
    );
    let untouched = BrowserSessionStore::load(conn, TENANT, SESSION)
        .await
        .expect("load");
    assert_eq!(untouched.status, SessionStatus::Active);
    assert_eq!(untouched.control_holder, ControlHolder::Agent);
    assert_eq!(
        BrowserSessionStore::list_for_target(conn, TENANT, TARGET)
            .await
            .expect("list")
            .len(),
        1
    );

    // A state that is not one the domain defines cannot reach the table at all: the schema's CHECK
    // refuses it, so the store never has to decide what `sleeping` means. The store's `Corrupt` variant
    // covers a row that predates such a constraint, which is not something a test can fabricate while the
    // constraint is doing its job -- so what is asserted is the guard itself.
    let refused = sqlx::query(
        "UPDATE browser_sessions SET status = 'sleeping' WHERE id = $1 AND tenant_id = $2",
    )
    .bind(SESSION)
    .bind(TENANT)
    .execute(&mut *conn)
    .await;
    assert!(
        refused.is_err(),
        "an undefined state was accepted: {refused:?}"
    );
    let still = BrowserSessionStore::load(conn, TENANT, SESSION)
        .await
        .expect("load");
    assert_eq!(still.status, SessionStatus::Active);

    common::drop_pool(&pool, &name).await;
}
