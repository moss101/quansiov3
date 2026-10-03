//! Terminal conditions, permission parking and the takeover race (ADP-002, DOMAIN.md §8.6.1 B and E).
//!
//! The runtime walks a sequence of native actions; these tests drive it against real PostgreSQL
//! (`QUANSIO_TEST_POSTGRES_URL`, else `BLOCKED_EXTERNAL`) with a scripted platform. What each one proves
//! is stated by a count the model could not influence: how many times the feed was asked for another
//! action, and how many actions reached the platform.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use quansio_machine::computer_use::bridge::{
    ComputerDispatchError, ComputerDispatcher, NativeComputerAction,
};
use quansio_machine::computer_use::config::SequenceLimits;
use quansio_machine::computer_use::permission::{
    ForegroundControl, GuideAction, Permission, PermissionState, PermissionStatus,
};
use quansio_machine::computer_use::receipt::{
    ActionReceipt, BridgeReport, ExecutionMethod, LedgerOutcome, ReceiptMethod, ReceiptOutcome,
};
use quansio_machine::computer_use::sequence::{
    ActionFeed, ComputerSequence, LimitKind, ListFeed, SequenceEnd, SettledAction, StopHandle,
    TerminalCondition,
};
use quansio_machine::computer_use::store::{ComputerControlError, ComputerControlStore};
use quansio_machine::computer_use::ComputerTier;
use serde_json::json;
use sqlx::PgPool;

mod common;
use common::computer::{
    click, clipboard_write, policy, prepare, read, type_text, ScriptedBridge, Step, AT, CALL_A,
    CALL_B, TARGET, TENANT,
};

fn limits(actions: usize, ms: u64, bytes: usize) -> SequenceLimits {
    SequenceLimits {
        max_actions: actions,
        max_duration: Duration::from_millis(ms),
        max_typed_bytes: bytes,
    }
}

/// A "model" that always has another click to propose, and counts how often it was asked.
struct EndlessFeed {
    asked: Arc<AtomicUsize>,
}

impl ActionFeed for EndlessFeed {
    fn next_action(&mut self, _settled: &[SettledAction]) -> Option<NativeComputerAction> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Some(click())
    }

    fn remaining(&self) -> usize {
        usize::MAX / 2
    }
}

fn endless() -> (EndlessFeed, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    (
        EndlessFeed {
            asked: asked.clone(),
        },
        asked,
    )
}

fn dispatcher(bridge: &ScriptedBridge) -> ComputerDispatcher<ScriptedBridge> {
    ComputerDispatcher::new(bridge.clone(), policy(ComputerTier::SystemKey))
}

async fn control(pool: &PgPool) -> quansio_machine::computer_use::store::ComputerControl {
    let mut conn = pool.acquire().await.expect("acquire");
    ComputerControlStore::load(&mut conn, TENANT, TARGET)
        .await
        .expect("load")
}

// ------------------------------------------------------------------ the four terminal conditions

#[tokio::test]
async fn a_user_stop_ends_the_sequence_and_the_model_is_not_asked_again() {
    let Some((name, pool)) = prepare("seq_stop").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    let stop = StopHandle::new();
    {
        let stop = stop.clone();
        bridge.on_perform(move |call| {
            if call == 2 {
                stop.stop();
            }
        });
    }
    let dispatcher = dispatcher(&bridge);
    let (feed, asked) = endless();
    let mut conn = pool.acquire().await.expect("acquire");
    let report = ComputerSequence::new(&dispatcher, stop)
        .with_limits(limits(100, 60_000, 1 << 20))
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::StoppedByUser)
    );
    assert_eq!(
        report.settled.len(),
        2,
        "the action in flight settled with its receipt"
    );
    assert_eq!(
        bridge.performed(),
        2,
        "nothing reached the platform after the stop"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "the model was not asked for a third action"
    );
    // The slot is free: the stop did not leave a half-held fence.
    assert!(control(&pool).await.active_tool_call_id.is_none());
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_permission_revoked_mid_sequence_stops_it_with_a_guide_to_restore_it() {
    let Some((name, pool)) = prepare("seq_revoked").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    {
        let handle = bridge.clone();
        bridge.on_perform(move |call| {
            if call == 2 {
                handle.set_permissions(
                    PermissionStatus::all_granted()
                        .with(Permission::Accessibility, PermissionState::Denied),
                );
            }
        });
    }
    let dispatcher = dispatcher(&bridge);
    let (feed, asked) = endless();
    let mut conn = pool.acquire().await.expect("acquire");
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .with_limits(limits(100, 60_000, 1 << 20))
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    let SequenceEnd::Terminal(condition) = &report.end else {
        panic!("expected a terminal condition, got {:?}", report.end);
    };
    assert_eq!(condition.code(), "COMPUTER_PERMISSION_REVOKED");
    let TerminalCondition::PermissionRevoked { attention } = condition else {
        panic!("{condition:?}");
    };
    assert_eq!(attention.permission, Some(Permission::Accessibility));
    assert_eq!(
        attention.guide,
        GuideAction::OpenSettings {
            pane: "Privacy_Accessibility".to_string()
        }
    );
    assert_eq!(
        bridge.performed(),
        2,
        "the revoked permission stopped action 3 before the platform"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        3,
        "action 3 was proposed, and refused, and the model was not asked again"
    );
    assert_eq!(report.skipped, 1 + usize::MAX / 2);
    assert!(condition.retryable() && condition.requires_new_observation());
    assert_eq!(condition.requires(), "permission_granted");
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_spent_limit_ends_the_sequence_for_each_kind_of_limit() {
    let Some((name, pool)) = prepare("seq_limits").await else {
        common::blocked_marker();
        return;
    };
    // Action count.
    let bridge = ScriptedBridge::new();
    let dispatcher = dispatcher(&bridge);
    let (feed, asked) = endless();
    let mut conn = pool.acquire().await.expect("acquire");
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .with_limits(limits(3, 60_000, 1 << 20))
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::LimitExceeded {
            limit: LimitKind::Actions
        })
    );
    assert_eq!(bridge.performed(), 3);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        4,
        "the fourth was proposed and refused; no fifth"
    );

    // Typed bytes: the second type action would pass the limit, so it is refused before it is sent.
    let bridge = ScriptedBridge::new();
    let dispatcher = self::dispatcher(&bridge);
    let feed = ListFeed::new([type_text("12345"), type_text("67890"), click()]);
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .with_limits(limits(10, 60_000, 8))
        .run(&mut conn, TENANT, TARGET, CALL_B, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::LimitExceeded {
            limit: LimitKind::TypedBytes
        })
    );
    assert_eq!(bridge.performed(), 1);
    assert_eq!(
        report.skipped, 2,
        "the refused action and the click behind it"
    );

    // Clipboard writes count as typed bytes too.
    let bridge = ScriptedBridge::new();
    let dispatcher = self::dispatcher(&bridge);
    let feed = ListFeed::new([clipboard_write("abcdefgh"), clipboard_write("i")]);
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .with_limits(limits(10, 60_000, 8))
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::LimitExceeded {
            limit: LimitKind::TypedBytes
        })
    );

    // Wall time: a platform call that takes longer than the limit ends the sequence at the next action.
    let bridge = ScriptedBridge::new();
    bridge.on_perform(|_| std::thread::sleep(Duration::from_millis(120)));
    let dispatcher = self::dispatcher(&bridge);
    let (feed, _) = endless();
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .with_limits(limits(100, 50, 1 << 20))
        .run(&mut conn, TENANT, TARGET, CALL_B, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::LimitExceeded {
            limit: LimitKind::Duration
        })
    );
    assert_eq!(bridge.performed(), 1);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_takeover_requested_during_an_action_ends_the_sequence_after_that_action() {
    let Some((name, pool)) = prepare("seq_takeover").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    // Action 2 blocks inside the platform call until the takeover has been requested, so the request
    // lands while the action is in flight rather than before or after it.
    let (inflight_tx, inflight_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let inflight_tx = Mutex::new(inflight_tx);
    bridge.on_perform(move |call| {
        if call == 2 {
            inflight_tx.lock().expect("tx").send(()).expect("signal");
            go_rx.lock().expect("rx").recv().expect("go");
        }
    });
    let (feed, asked) = endless();
    let mut conn = pool.acquire().await.expect("acquire");
    let task_pool = pool.clone();
    let task_bridge = bridge.clone();
    let sequence = tokio::spawn(async move {
        let dispatcher = ComputerDispatcher::new(task_bridge, policy(ComputerTier::SystemKey));
        let mut conn = task_pool.acquire().await.expect("acquire");
        ComputerSequence::new(&dispatcher, StopHandle::new())
            .with_limits(limits(100, 60_000, 1 << 20))
            .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
            .await
            .expect("sequence")
    });

    tokio::task::spawn_blocking(move || inflight_rx.recv().expect("action 2 is in flight"))
        .await
        .expect("wait");
    let pending = ComputerControlStore::request_takeover(&mut conn, TENANT, TARGET, 1)
        .await
        .expect("request takeover");
    assert!(pending.takeover_pending);
    assert_eq!(
        pending.active_tool_call_id.as_deref(),
        Some(CALL_A),
        "action 2 still holds the slot"
    );
    // The person cannot be given the machine while an action holds the slot.
    assert!(matches!(
        ComputerControlStore::complete_takeover(&mut conn, TENANT, TARGET, 1, AT).await,
        Err(ComputerControlError::ActionInFlight(_))
    ));
    // The refused call returned with its transaction open (sqlx rolls an abandoned transaction back
    // when the connection is next used), and that transaction still holds the row lock the in-flight
    // action needs in order to finish. Use the connection once so the rollback is sent; otherwise this
    // test waits for itself.
    ComputerControlStore::load(&mut conn, TENANT, TARGET)
        .await
        .expect("use the connection");
    go_tx.send(()).expect("release action 2");
    let report = sequence.await.expect("join");

    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::TakeoverActive)
    );
    assert_eq!(
        report.settled.len(),
        2,
        "the action that was in flight finished and kept its receipt"
    );
    assert_eq!(
        bridge.performed(),
        2,
        "no input after the takeover was requested"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "the model was not asked for a third action"
    );

    // The slot was released by the finished action, so the takeover can now complete, and a new
    // sequence started with the stale generation is refused before it reaches the platform.
    let owned = ComputerControlStore::complete_takeover(&mut conn, TENANT, TARGET, 1, AT)
        .await
        .expect("complete takeover");
    assert_eq!(owned.generation, 2);
    let dispatcher = dispatcher(&bridge);
    let refused = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_B,
            1,
            ListFeed::new([click()]),
        )
        .await
        .expect("sequence");
    assert_eq!(
        refused.end,
        SequenceEnd::Terminal(TerminalCondition::TakeoverActive)
    );
    assert_eq!(bridge.performed(), 2);

    // After an explicit handback the generation is 3: the old sequence's generation stays refused (what
    // the model saw is stale), and a sequence at the new generation runs.
    ComputerControlStore::handback(&mut conn, TENANT, TARGET, 2, AT)
        .await
        .expect("handback");
    // The stale sequence is refused before the model is even asked for an action: the count, not the
    // end state, is what separates "noticed the changed generation" from "tried and was refused".
    let (feed, asked) = endless();
    let stale = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(&mut conn, TENANT, TARGET, CALL_B, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        stale.end,
        SequenceEnd::Terminal(TerminalCondition::TakeoverActive)
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "a sequence from an older generation was still asked for actions"
    );
    let fresh = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_B,
            3,
            ListFeed::new([click()]),
        )
        .await
        .expect("sequence");
    assert_eq!(fresh.end, SequenceEnd::Completed);
    assert_eq!(bridge.performed(), 3);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_takeover_that_lands_between_the_check_and_the_action_is_still_a_takeover() {
    let Some((name, pool)) = prepare("seq_takeover_gap").await else {
        common::blocked_marker();
        return;
    };
    // The control row is read, then the model is asked for its next action, and only then does the
    // action begin. A takeover landing in that gap must surface as the terminal condition, not as a
    // database error and not as one more action.
    struct TakeoverOnAsk {
        pool: PgPool,
        asked: usize,
    }
    impl ActionFeed for TakeoverOnAsk {
        fn next_action(&mut self, _settled: &[SettledAction]) -> Option<NativeComputerAction> {
            self.asked += 1;
            if self.asked == 2 {
                // The feed is the model's side; the person's takeover arrives while it thinks.
                let pool = self.pool.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut conn = pool.acquire().await.expect("acquire");
                        ComputerControlStore::request_takeover(&mut conn, TENANT, TARGET, 1)
                            .await
                            .expect("takeover");
                    });
                });
            }
            Some(click())
        }

        fn remaining(&self) -> usize {
            0
        }
    }
    let bridge = ScriptedBridge::new();
    let dispatcher = dispatcher(&bridge);
    let feed = TakeoverOnAsk {
        pool: pool.clone(),
        asked: 0,
    };
    let mut conn = pool.acquire().await.expect("acquire");
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .with_limits(limits(10, 60_000, 1 << 20))
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::TakeoverActive)
    );
    assert_eq!(
        bridge.performed(),
        1,
        "the second click never reached the platform"
    );
    assert_eq!(report.skipped, 1);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ the model cannot continue a sequence

#[tokio::test]
async fn nothing_a_model_returns_can_continue_a_sequence_that_has_ended() {
    let Some((name, pool)) = prepare("seq_closed").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    let dispatcher = dispatcher(&bridge);
    let stop = StopHandle::new();
    stop.stop();
    // The stop is already raised when the sequence starts: not one action is proposed, let alone run.
    let (feed, asked) = endless();
    let mut conn = pool.acquire().await.expect("acquire");
    let report = ComputerSequence::new(&dispatcher, stop.clone())
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Terminal(TerminalCondition::StoppedByUser)
    );
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert_eq!(bridge.performed(), 0);
    // A raised stop cannot be lowered: the flag has no way back.
    assert!(stop.is_stopped());
    // The wire form a model is shown says what happened and what it must do first.
    let wire = report.to_json().expect("json");
    assert_eq!(wire["end"]["status"], json!("terminal"));
    assert_eq!(
        wire["end"]["error"]["code"],
        json!("COMPUTER_STOPPED_BY_USER")
    );
    assert_eq!(wire["end"]["error"]["retryable"], json!(false));
    assert_eq!(wire["end"]["error"]["requires"], json!("user_resume"));
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[test]
fn the_terminal_conditions_carry_the_flags_domain_md_states() {
    // DOMAIN.md §15 "Computer error flags".
    let attention = quansio_machine::computer_use::permission::AttentionRequest {
        reason: quansio_machine::computer_use::permission::AttentionReason::PermissionMissing,
        permission: Some(Permission::Accessibility),
        status: Some(PermissionState::Denied),
        guide: GuideAction::PauseInput,
    };
    let rows = [
        (
            TerminalCondition::TakeoverActive,
            "COMPUTER_TAKEOVER_ACTIVE",
            true,
            true,
            "handback",
        ),
        (
            TerminalCondition::PermissionRevoked { attention },
            "COMPUTER_PERMISSION_REVOKED",
            true,
            true,
            "permission_granted",
        ),
        (
            TerminalCondition::LimitExceeded {
                limit: LimitKind::Actions,
            },
            "COMPUTER_LIMIT_EXCEEDED",
            false,
            true,
            "smaller_sequence",
        ),
        (
            TerminalCondition::StoppedByUser,
            "COMPUTER_STOPPED_BY_USER",
            false,
            true,
            "user_resume",
        ),
    ];
    for (condition, code, retryable, observe, requires) in rows {
        assert_eq!(condition.code(), code);
        assert_eq!(condition.retryable(), retryable, "{code}");
        assert_eq!(condition.requires_new_observation(), observe, "{code}");
        assert_eq!(condition.requires(), requires, "{code}");
        let wire = condition.to_json();
        assert_eq!(wire["code"], json!(code));
        assert_eq!(wire["retryable"], json!(retryable));
    }
}

// ------------------------------------------------------------------ parking on a missing permission

#[tokio::test]
async fn a_missing_permission_parks_the_run_instead_of_inviting_a_retry() {
    let Some((name, pool)) = prepare("seq_parked").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    bridge.set_permissions(
        PermissionStatus::all_granted()
            .with(Permission::Accessibility, PermissionState::NotDetermined),
    );
    let dispatcher = dispatcher(&bridge);
    let mut conn = pool.acquire().await.expect("acquire");

    // A model that keeps proposing is asked exactly once: the first action parks the run.
    let (feed, asked) = endless();
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    let SequenceEnd::Parked(attention) = &report.end else {
        panic!("expected the run to park, got {:?}", report.end);
    };
    assert_eq!(attention.code(), "COMPUTER_PERMISSION_REQUIRED");
    assert_eq!(attention.permission, Some(Permission::Accessibility));
    assert_eq!(attention.status, Some(PermissionState::NotDetermined));
    assert_eq!(
        attention.guide,
        GuideAction::OpenSettings {
            pane: "Privacy_Accessibility".to_string()
        }
    );
    let wire = attention.to_json();
    assert_eq!(wire["notification_kind"], json!("blocked"));
    assert_eq!(wire["resume_command"], json!("ResumeRun"));
    assert_eq!(wire["guide"]["pane"], json!("Privacy_Accessibility"));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(bridge.performed(), 0, "nothing reached the platform");
    assert!(report.settled.is_empty());
    assert!(
        control(&pool).await.active_tool_call_id.is_none(),
        "no dispatch slot was taken for an action that could not run"
    );

    // Asking again — what a model that retries would do — gets the same answer and still reaches nothing.
    for _ in 0..3 {
        let again = ComputerSequence::new(&dispatcher, StopHandle::new())
            .run(
                &mut conn,
                TENANT,
                TARGET,
                CALL_A,
                1,
                ListFeed::new([click()]),
            )
            .await
            .expect("sequence");
        assert!(matches!(again.end, SequenceEnd::Parked(_)));
    }
    assert_eq!(bridge.performed(), 0);

    // The person grants it and resumes: the same action now runs.
    bridge.set_permissions(PermissionStatus::all_granted());
    let resumed = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([click()]),
        )
        .await
        .expect("sequence");
    assert_eq!(resumed.end, SequenceEnd::Completed);
    assert_eq!(bridge.performed(), 1);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_screenshot_needs_screen_recording_and_a_restricted_permission_asks_an_administrator() {
    let Some((name, pool)) = prepare("seq_screen").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    bridge.set_permissions(
        PermissionStatus::all_granted().with(Permission::ScreenRecording, PermissionState::Denied),
    );
    let dispatcher = dispatcher(&bridge);
    let mut conn = pool.acquire().await.expect("acquire");
    // A read without a screenshot needs only Accessibility.
    let plain = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([read(false)]),
        )
        .await
        .expect("sequence");
    assert_eq!(plain.end, SequenceEnd::Completed);
    // With a screenshot it parks on Screen Recording.
    let shot = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([read(true)]),
        )
        .await
        .expect("sequence");
    let SequenceEnd::Parked(attention) = shot.end else {
        panic!("expected a park");
    };
    assert_eq!(attention.permission, Some(Permission::ScreenRecording));
    assert_eq!(
        attention.guide,
        GuideAction::OpenSettings {
            pane: "Privacy_ScreenCapture".to_string()
        }
    );
    // A clipboard write needs no OS permission at all.
    bridge.set_permissions(
        PermissionStatus::all_granted().with(Permission::Accessibility, PermissionState::Denied),
    );
    let clip = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([clipboard_write("x")]),
        )
        .await
        .expect("sequence");
    assert_eq!(clip.end, SequenceEnd::Completed);
    // Restricted by policy: the person cannot grant it, so the guide asks for an administrator.
    bridge.set_permissions(
        PermissionStatus::all_granted()
            .with(Permission::Accessibility, PermissionState::Restricted),
    );
    let restricted = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([click()]),
        )
        .await
        .expect("sequence");
    let SequenceEnd::Parked(attention) = restricted.end else {
        panic!("expected a park");
    };
    assert_eq!(attention.guide, GuideAction::ContactAdministrator);
    assert_eq!(attention.to_json()["guide"].get("pane"), None);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_lapsed_foreground_grant_asks_the_person_to_pause_input_and_never_opens_settings() {
    let Some((name, pool)) = prepare("seq_lapsed").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    {
        let handle = bridge.clone();
        bridge.on_perform(move |call| {
            if call == 1 {
                // The person starts using the keyboard and pointer after the first action.
                handle.set_foreground_control(ForegroundControl::Lapsed);
            }
        });
    }
    let dispatcher = dispatcher(&bridge);
    let mut conn = pool.acquire().await.expect("acquire");
    let (feed, asked) = endless();
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    let SequenceEnd::Parked(attention) = &report.end else {
        panic!("expected a park, got {:?}", report.end);
    };
    assert_eq!(attention.code(), "COMPUTER_FOREGROUND_CONTROL_LAPSED");
    assert_eq!(attention.guide, GuideAction::PauseInput);
    assert_eq!(
        attention.permission, None,
        "no permission is missing, so Settings is not the answer"
    );
    assert_eq!(attention.to_json()["guide"].get("pane"), None);
    assert_eq!(report.settled.len(), 1);
    assert_eq!(bridge.performed(), 1);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    // A clipboard action does not drive the foreground, so a lapsed grant does not stop it.
    let clip = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_B,
            1,
            ListFeed::new([clipboard_write("x")]),
        )
        .await
        .expect("sequence");
    assert_eq!(clip.end, SequenceEnd::Completed);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn an_unreadable_permission_status_refuses_rather_than_assuming_granted() {
    let Some((name, pool)) = prepare("seq_status_fail").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    bridge.fail_status(true);
    let dispatcher = dispatcher(&bridge);
    let mut conn = pool.acquire().await.expect("acquire");
    let error = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([click()]),
        )
        .await
        .expect_err("fails closed");
    assert!(matches!(
        error,
        ComputerDispatchError::PermissionStatusUnavailable(_)
    ));
    assert_eq!(bridge.performed(), 0);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ receipts and recovery inside a sequence

#[tokio::test]
async fn a_receipt_that_says_the_effect_is_absent_halts_the_sequence_without_being_a_terminal_condition(
) {
    let Some((name, pool)) = prepare("seq_halt").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    bridge.push(Step::Report(BridgeReport {
        output: json!({ "typed": true }),
        receipt: ActionReceipt::applied(ReceiptMethod::HidEvent, ExecutionMethod::Foreground),
    }));
    bridge.push(Step::Report(BridgeReport {
        output: json!({ "typed": false }),
        receipt: ActionReceipt::not_applied(ReceiptMethod::HidEvent, ExecutionMethod::Foreground),
    }));
    let dispatcher = dispatcher(&bridge);
    let mut conn = pool.acquire().await.expect("acquire");
    let feed = ListFeed::new([type_text("a"), type_text("b"), click(), click()]);
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(&mut conn, TENANT, TARGET, CALL_A, 1, feed)
        .await
        .expect("sequence");
    assert_eq!(
        report.end,
        SequenceEnd::Halted {
            outcome: ReceiptOutcome::NotApplied
        }
    );
    assert_eq!(bridge.performed(), 2);
    assert_eq!(report.skipped, 2);
    // The ToolCall's ledger outcome is the least certain of its actions.
    assert_eq!(report.ledger_outcome(), LedgerOutcome::OutcomeUnknown);
    let wire = report.to_json().expect("json");
    assert_eq!(
        wire["actions"][0]["receipt"]["effect_verified"],
        json!(true)
    );
    assert_eq!(
        wire["actions"][1]["receipt"]["effect_verified"],
        json!(false)
    );
    assert_eq!(wire["end"]["status"], json!("halted"));
    assert_eq!(wire["ledger_outcome"], json!("OUTCOME_UNKNOWN"));
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn an_unverified_action_does_not_stop_a_sequence_but_is_never_settled_as_success() {
    let Some((name, pool)) = prepare("seq_unverified").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    let dispatcher = dispatcher(&bridge);
    let mut conn = pool.acquire().await.expect("acquire");
    let report = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([click(), click(), click()]),
        )
        .await
        .expect("sequence");
    assert_eq!(report.end, SequenceEnd::Completed);
    assert_eq!(report.settled.len(), 3);
    for settled in &report.settled {
        assert_eq!(settled.result.receipt.effect_verified, None);
    }
    assert_eq!(report.ledger_outcome(), LedgerOutcome::OutcomeUnknown);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_crash_mid_sequence_leaves_the_call_active_and_a_restart_does_not_retry_it() {
    let Some((name, pool)) = prepare("seq_crash").await else {
        common::blocked_marker();
        return;
    };
    let bridge = ScriptedBridge::new();
    bridge.push(Step::Report(common::computer::applied_report()));
    bridge.push(Step::Fail("native_disconnect"));
    let dispatcher = dispatcher(&bridge);
    let mut conn = pool.acquire().await.expect("acquire");
    let error = ComputerSequence::new(&dispatcher, StopHandle::new())
        .run(
            &mut conn,
            TENANT,
            TARGET,
            CALL_A,
            1,
            ListFeed::new([click(), click(), click()]),
        )
        .await
        .expect_err("the bridge disconnected after dispatch");
    assert!(matches!(
        error,
        ComputerDispatchError::OutcomeUnknown { .. }
    ));
    assert_eq!(bridge.performed(), 2);
    drop(conn);

    // A restart: new connection, new dispatcher, new bridge. The durable row still says action 2 is in
    // doubt, and a fresh sequence is refused rather than re-sending anything.
    let mut restarted = pool.acquire().await.expect("reacquire");
    let after = ComputerControlStore::load(&mut restarted, TENANT, TARGET)
        .await
        .expect("load");
    assert_eq!(after.active_tool_call_id.as_deref(), Some(CALL_A));
    let fresh_bridge = ScriptedBridge::new();
    let fresh = ComputerDispatcher::new(fresh_bridge.clone(), policy(ComputerTier::SystemKey));
    // The same ToolCall redelivered, and a different one: neither reaches the platform.
    for call in [CALL_A, CALL_B] {
        let refused = ComputerSequence::new(&fresh, StopHandle::new())
            .run(
                &mut restarted,
                TENANT,
                TARGET,
                call,
                1,
                ListFeed::new([click()]),
            )
            .await
            .expect_err("refused");
        assert!(
            matches!(
                refused,
                ComputerDispatchError::Control(ComputerControlError::ActionInFlight(_))
            ),
            "{call}: {refused:?}"
        );
    }
    assert_eq!(
        fresh_bridge.performed(),
        0,
        "no blind retry of an unknown effect"
    );
    // Reconciliation releases it; only then may anything run.
    ComputerControlStore::finish_action(&mut restarted, TENANT, TARGET, CALL_A, 1)
        .await
        .expect("reconcile");
    let resumed = ComputerSequence::new(&fresh, StopHandle::new())
        .run(
            &mut restarted,
            TENANT,
            TARGET,
            CALL_B,
            1,
            ListFeed::new([click()]),
        )
        .await
        .expect("sequence");
    assert_eq!(resumed.end, SequenceEnd::Completed);
    drop(restarted);
    common::drop_pool(&pool, &name).await;
}
