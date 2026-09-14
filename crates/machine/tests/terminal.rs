//! Terminal sessions against real PostgreSQL (EXEC-006).
//!
//! The acceptance statement is that a terminal replay resumes from the durable cursor without
//! duplicating the command. The worker's half of that decision is tested in
//! `crates/qworkerd/tests/tools.rs`; this suite proves the *durable* half — that the cursor and the
//! command id survive the worker, that a reconnect is therefore a replay, and that the cursor cannot be
//! walked backwards.
//!
//! Every case drives the shipped `TerminalStore` against a scratch database. When the environment
//! provides no database the suite reports `BLOCKED_EXTERNAL` and returns, so a missing dev stack is
//! never a pass.

mod common;

use quansio_machine::control::terminal::{Attachment, SessionStatus, TerminalStore};
use quansio_machine::control::{MachineControl, NewTarget, Substrate, TargetClass};
use sqlx::{PgConnection, PgPool};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const SESSION: &str = "tsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const COMMAND: &str = "tc_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_COMMAND: &str = "tc_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";

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

async fn open(conn: &mut PgConnection) -> quansio_machine::control::terminal::TerminalSession {
    TerminalStore::open(conn, TENANT, SESSION, TARGET, None, Some("pty-1"))
        .await
        .expect("open session")
}

// ------------------------------------------------------------------ the durable cursor

#[tokio::test]
async fn a_reconnect_replays_from_the_durable_cursor_instead_of_running_the_command_again() {
    let name = common::scratch_name("terminal_replay");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    let session = open(conn).await;
    assert_eq!(session.status, SessionStatus::Open);
    assert_eq!(session.cursor, 0);
    assert_eq!(session.last_command_id, None);
    assert_eq!(session.pty_ref.as_deref(), Some("pty-1"));

    // The first attach dispatches: nothing has run on this session yet.
    let first = TerminalStore::attach(conn, TENANT, SESSION, COMMAND)
        .await
        .expect("attach");
    assert_eq!(
        first,
        Attachment::Dispatch {
            command_id: COMMAND.to_string(),
            from: 0,
        }
    );
    assert!(first.is_dispatch());

    // The command ran and produced output up to offset 12, which the client consumed. That offset is
    // durable: it is not this connection's state.
    assert_eq!(
        TerminalStore::advance(conn, TENANT, SESSION, 12)
            .await
            .expect("advance"),
        12
    );

    // A *different* connection — a reconnecting worker, a different process — sees both.
    let mut second = pool.acquire().await.expect("acquire");
    let reloaded = TerminalStore::load(&mut second, TENANT, SESSION)
        .await
        .expect("reload");
    assert_eq!(reloaded.cursor, 12);
    assert_eq!(reloaded.last_command_id.as_deref(), Some(COMMAND));

    // The reconnect attaches the same command. It is a replay, from the durable cursor, and the store
    // did not record a second dispatch of it.
    let replay = TerminalStore::attach(&mut second, TENANT, SESSION, COMMAND)
        .await
        .expect("reattach");
    assert_eq!(
        replay,
        Attachment::Replay {
            command_id: COMMAND.to_string(),
            from: 12,
        },
        "the reconnect re-dispatched a command that had already run"
    );
    assert!(!replay.is_dispatch());
    assert_eq!(replay.from(), 12);

    // Replaying again is still a replay, and the cursor has not moved on its own.
    assert!(matches!(
        TerminalStore::attach(&mut second, TENANT, SESSION, COMMAND)
            .await
            .expect("reattach again"),
        Attachment::Replay { from: 12, .. }
    ));

    // A new command on the same session is a dispatch, starting at the cursor — so a session is reusable
    // and the history does not confuse the next call.
    let next = TerminalStore::attach(&mut second, TENANT, SESSION, OTHER_COMMAND)
        .await
        .expect("attach the next command");
    assert_eq!(
        next,
        Attachment::Dispatch {
            command_id: OTHER_COMMAND.to_string(),
            from: 12,
        }
    );
    // And now *that* command is the one a reconnect replays, not the first.
    assert!(matches!(
        TerminalStore::attach(&mut second, TENANT, SESSION, OTHER_COMMAND)
            .await
            .expect("reattach"),
        Attachment::Replay { from: 12, .. }
    ));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn the_cursor_only_moves_forward_and_a_session_that_is_not_open_refuses() {
    let name = common::scratch_name("terminal_cursor");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    open(conn).await;

    // Forward, and idempotently at the same offset.
    assert_eq!(
        TerminalStore::advance(conn, TENANT, SESSION, 8)
            .await
            .expect("advance"),
        8
    );
    assert_eq!(
        TerminalStore::advance(conn, TENANT, SESSION, 8)
            .await
            .expect("same"),
        8
    );
    assert_eq!(
        TerminalStore::advance(conn, TENANT, SESSION, 40)
            .await
            .expect("advance"),
        40
    );

    // A client asking to re-read bytes it has already consumed is refused, with both offsets named, so
    // it cannot make the session re-send what it has.
    let refusal = TerminalStore::advance(conn, TENANT, SESSION, 39)
        .await
        .expect_err("a backwards cursor must be refused");
    assert!(
        matches!(
            refusal,
            quansio_machine::control::terminal::TerminalError::CursorBehind {
                requested: 39,
                durable: 40
            }
        ),
        "{refusal:?}"
    );
    assert_eq!(
        TerminalStore::load(conn, TENANT, SESSION)
            .await
            .expect("load")
            .cursor,
        40,
        "a refused advance moved the cursor"
    );

    // Closing is a state, and a closed session refuses an attach rather than replaying or dispatching.
    let closed = TerminalStore::close(conn, TENANT, SESSION, SessionStatus::Closed)
        .await
        .expect("close");
    assert_eq!(closed.status, SessionStatus::Closed);
    let refusal = TerminalStore::attach(conn, TENANT, SESSION, COMMAND)
        .await
        .expect_err("a closed session must refuse");
    assert!(
        matches!(
            refusal,
            quansio_machine::control::terminal::TerminalError::NotOpen {
                status: "closed",
                ..
            }
        ),
        "{refusal:?}"
    );
    // A lost session behaves the same way: the conduit died, so a command that "ran" would be a guess.
    TerminalStore::close(conn, TENANT, SESSION, SessionStatus::Lost)
        .await
        .expect("mark lost");
    assert!(matches!(
        TerminalStore::attach(conn, TENANT, SESSION, COMMAND).await,
        Err(quansio_machine::control::terminal::TerminalError::NotOpen { status: "lost", .. })
    ));
    // Output produced before the session closed is still recorded, so closing does not lose bytes.
    assert_eq!(
        TerminalStore::advance(conn, TENANT, SESSION, 55)
            .await
            .expect("advance"),
        55
    );

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_session_is_scoped_to_its_tenant_and_target() {
    let name = common::scratch_name("terminal_scope");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    open(conn).await;

    // Another tenant cannot see, attach to, advance or close it.
    assert!(matches!(
        TerminalStore::load(conn, OTHER_TENANT, SESSION).await,
        Err(quansio_machine::control::terminal::TerminalError::NotFound(
            _
        ))
    ));
    assert!(matches!(
        TerminalStore::attach(conn, OTHER_TENANT, SESSION, COMMAND).await,
        Err(quansio_machine::control::terminal::TerminalError::NotFound(
            _
        ))
    ));
    assert!(matches!(
        TerminalStore::advance(conn, OTHER_TENANT, SESSION, 1).await,
        Err(quansio_machine::control::terminal::TerminalError::NotFound(
            _
        ))
    ));
    assert!(matches!(
        TerminalStore::close(conn, OTHER_TENANT, SESSION, SessionStatus::Closed).await,
        Err(quansio_machine::control::terminal::TerminalError::NotFound(
            _
        ))
    ));
    assert!(TerminalStore::list_for_target(conn, OTHER_TENANT, TARGET)
        .await
        .expect("list")
        .is_empty());
    // The session is untouched by all of that.
    let session = TerminalStore::load(conn, TENANT, SESSION)
        .await
        .expect("load");
    assert_eq!(session.status, SessionStatus::Open);
    assert_eq!(session.cursor, 0);
    assert!(session.last_command_id.is_none());

    // And the listing is the target's, not the tenant's.
    assert_eq!(
        TerminalStore::list_for_target(conn, TENANT, TARGET)
            .await
            .expect("list")
            .len(),
        1
    );

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_session_cannot_be_opened_on_a_target_that_does_not_exist_or_a_bad_id() {
    let name = common::scratch_name("terminal_open");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // A target the tenant does not have is refused rather than creating a dangling session.
    assert!(matches!(
        TerminalStore::open(
            conn,
            TENANT,
            SESSION,
            "tgt_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ",
            None,
            None
        )
        .await,
        Err(quansio_machine::control::terminal::TerminalError::TargetNotFound(_))
    ));
    // And an id that is not a `tsn_` id is refused before anything is written.
    assert!(matches!(
        TerminalStore::open(
            conn,
            TENANT,
            "sess_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
            TARGET,
            None,
            None
        )
        .await,
        Err(quansio_machine::control::terminal::TerminalError::SessionIdInvalid(_))
    ));
    assert!(TerminalStore::list_for_target(conn, TENANT, TARGET)
        .await
        .expect("list")
        .is_empty());

    // A cursor stored as something that is not an offset is refused rather than read as zero, which
    // would silently re-send a whole session.
    open(conn).await;
    sqlx::query(
        "UPDATE terminal_sessions SET cursor = 'not-a-number' WHERE id = $1 AND tenant_id = $2",
    )
    .bind(SESSION)
    .bind(TENANT)
    .execute(&mut *conn)
    .await
    .expect("corrupt the cursor");
    assert!(matches!(
        TerminalStore::load(conn, TENANT, SESSION).await,
        Err(quansio_machine::control::terminal::TerminalError::Corrupt { .. })
    ));
    assert!(matches!(
        TerminalStore::advance(conn, TENANT, SESSION, 1).await,
        Err(quansio_machine::control::terminal::TerminalError::Corrupt { .. })
    ));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn two_reconnects_with_the_same_command_cannot_both_be_told_to_run_it() {
    let name = common::scratch_name("terminal_race");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    open(conn).await;
    TerminalStore::attach(conn, TENANT, SESSION, COMMAND)
        .await
        .expect("first dispatch");

    // Two connections attach the same command concurrently. Exactly one may be told to run it — and since
    // it has already run, that one is the replay: neither is a dispatch.
    let mut left = pool.acquire().await.expect("acquire");
    let mut right = pool.acquire().await.expect("acquire");
    let (left, right) = tokio::join!(
        TerminalStore::attach(&mut left, TENANT, SESSION, COMMAND),
        TerminalStore::attach(&mut right, TENANT, SESSION, COMMAND),
    );
    let left = left.expect("left attach");
    let right = right.expect("right attach");
    assert!(
        !left.is_dispatch() && !right.is_dispatch(),
        "{left:?} {right:?}"
    );
    assert_eq!(left, right);

    common::drop_pool(&pool, &name).await;
}
