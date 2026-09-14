//! Computer-use policy: tiers, fail-closed identity and the takeover fence (EXEC-010).
//!
//! The native half of EXEC-010 is `native/macos`, covered by its own Swift suite against the live system
//! (`swift test --package-path native/macos`). This suite covers the half that decides: which tier an
//! action needs, that an application nobody vouched for is refused whatever the grant, and that a human
//! takeover cannot interleave with agent input.

use quansio_machine::computer_use::{
    store::{ComputerControlError, ComputerControlStore},
    AppIdentity, ComputerGrant, ComputerTier, ComputerUsePolicy, KnownApps, MachineHolder, Refusal,
};
use quansio_machine::control::{MachineControl, NewTarget, Substrate, TargetClass};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Barrier;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const CALL_A: &str = "tc_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const CALL_B: &str = "tc_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";
const AT: &str = "2026-09-14T10:00:00Z";

/// An application the runtime recognises.
fn known_app() -> AppIdentity {
    AppIdentity::new("com.apple.TextEdit", "TextEdit", 4242)
}

/// One nobody vouched for.
fn stranger() -> AppIdentity {
    AppIdentity::new("com.example.unknown", "Something Else", 9999)
}

fn policy(granted: ComputerTier, known: &[&str]) -> ComputerUsePolicy {
    ComputerUsePolicy::new(KnownApps::of(known.to_vec()), ComputerGrant::up_to(granted))
}

// ------------------------------------------------------------------ permission tiers

#[test]
fn a_tier_is_a_grant_and_a_run_cannot_reach_past_it() {
    // A read-only run may read and may not type, click, touch the clipboard or send system keys.
    let reader = policy(ComputerTier::Read, &["com.apple.TextEdit"]);
    assert_eq!(
        reader.authorize(ComputerTier::Read, Some(&known_app())),
        Ok(())
    );
    for tier in [
        ComputerTier::Click,
        ComputerTier::Type,
        ComputerTier::Clipboard,
        ComputerTier::SystemKey,
    ] {
        assert_eq!(
            reader.authorize(tier, Some(&known_app())),
            Err(Refusal::TierNotGranted {
                tier: tier.as_str(),
                granted: "read",
            }),
            "{tier:?} must not be reachable from a read grant"
        );
    }

    // A grant reaches exactly its own rung and the ones below it.
    let typer = policy(ComputerTier::Type, &["com.apple.TextEdit"]);
    for (tier, allowed) in [
        (ComputerTier::Read, true),
        (ComputerTier::Click, true),
        (ComputerTier::Type, true),
        (ComputerTier::Clipboard, false),
        (ComputerTier::SystemKey, false),
    ] {
        assert_eq!(
            typer.authorize(tier, Some(&known_app())).is_ok(),
            allowed,
            "{tier:?} with a type grant"
        );
    }

    // The tiers are ordered, which is what makes `covers` meaningful rather than a list to maintain.
    assert!(ComputerTier::Read < ComputerTier::Click);
    assert!(ComputerTier::Click < ComputerTier::Type);
    assert!(ComputerTier::Type < ComputerTier::Clipboard);
    assert!(ComputerTier::Clipboard < ComputerTier::SystemKey);
    assert_eq!(
        ComputerTier::parse("system_key"),
        Some(ComputerTier::SystemKey)
    );
    assert_eq!(ComputerTier::parse("teleport"), None);
    assert!(!ComputerGrant::read_only().covers(ComputerTier::Click));
    assert_eq!(ComputerGrant::read_only().highest(), ComputerTier::Read);
    // Every tier names the tool that acts at it, so a refusal can say which call was refused.
    for tier in [
        ComputerTier::Read,
        ComputerTier::Click,
        ComputerTier::Type,
        ComputerTier::Clipboard,
        ComputerTier::SystemKey,
    ] {
        assert!(!tier.tools().is_empty(), "{tier:?} names no tool");
    }
}

// ------------------------------------------------------------------ app identity

#[test]
fn an_application_the_runtime_does_not_recognise_is_refused_at_every_tier() {
    // Fail closed means there is no tier at which guessing about the foreground app is safe: the identity
    // is what the decision is about, so an unknown app is refused before the tier is even considered --
    // a run granted every tier included.
    let everything = policy(ComputerTier::SystemKey, &["com.apple.TextEdit"]);
    assert_eq!(everything.grant().highest(), ComputerTier::SystemKey);
    for tier in [
        ComputerTier::Read,
        ComputerTier::Click,
        ComputerTier::Type,
        ComputerTier::Clipboard,
        ComputerTier::SystemKey,
    ] {
        assert_eq!(
            everything.authorize(tier, Some(&stranger())),
            Err(Refusal::UnknownApp {
                bundle_id: "com.example.unknown".to_string(),
            }),
            "{tier:?} was allowed on an unrecognised app"
        );
    }

    // A missing foreground application is its own refusal rather than being treated as unknown.
    assert_eq!(
        everything.authorize(ComputerTier::Read, None),
        Err(Refusal::NoForegroundApp)
    );

    // A recognised application is allowed, so the refusals above are the identity rule and not a broken
    // policy.
    assert_eq!(
        everything.authorize(ComputerTier::SystemKey, Some(&known_app())),
        Ok(())
    );

    // Identity matches on the bundle identifier, not the display name: a name is not an identity, and an
    // application can call itself anything.
    let impostor = AppIdentity::new("com.example.unknown", "TextEdit", 1);
    assert!(matches!(
        everything.authorize(ComputerTier::Read, Some(&impostor)),
        Err(Refusal::UnknownApp { .. })
    ));

    // The default set recognises nothing at all, which is what an uninformed run must get.
    let uninformed = ComputerUsePolicy::new(
        KnownApps::none(),
        ComputerGrant::up_to(ComputerTier::SystemKey),
    );
    assert!(uninformed.known().is_empty());
    assert_eq!(uninformed.known().len(), 0);
    assert!(matches!(
        uninformed.authorize(ComputerTier::Read, Some(&known_app())),
        Err(Refusal::UnknownApp { .. })
    ));
    let listed = KnownApps::of(["a.b", "c.d"]);
    assert_eq!(listed.len(), 2);
    assert!(listed.recognises("a.b"));
    assert!(!listed.recognises("e.f"));
}

// ------------------------------------------------------------------ durable takeover and recovery

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE).await;
    let mut conn = pool.acquire().await.expect("acquire");
    MachineControl::register(
        &mut conn,
        TENANT,
        &NewTarget {
            id: TARGET.to_string(),
            workspace_id: WORKSPACE.to_string(),
            class: TargetClass::PersistentWorkspaceComputer,
            substrate: Substrate::LocalCapsuleMacos,
            image_digest: None,
            desired_state: None,
        },
    )
    .await
    .expect("register target");
    ComputerControlStore::open(&mut conn, TENANT, TARGET, None)
        .await
        .expect("open control");
    drop(conn);
    Some((name, pool))
}

#[tokio::test]
async fn takeover_fences_input_survives_restart_and_requires_explicit_handback() {
    let Some((name, pool)) = prepare("computer_recovery").await else {
        common::blocked_marker();
        return;
    };
    let mut conn = pool.acquire().await.expect("acquire");
    let active = ComputerControlStore::begin_action(&mut conn, TENANT, TARGET, CALL_A, 1)
        .await
        .expect("begin action");
    assert_eq!(active.active_tool_call_id.as_deref(), Some(CALL_A));
    let pending = ComputerControlStore::request_takeover(&mut conn, TENANT, TARGET, 1)
        .await
        .expect("request takeover");
    assert!(pending.takeover_pending);
    assert!(matches!(
        ComputerControlStore::begin_action(&mut conn, TENANT, TARGET, CALL_B, 1).await,
        Err(ComputerControlError::TakeoverPending)
    ));
    assert!(matches!(
        ComputerControlStore::complete_takeover(&mut conn, TENANT, TARGET, 1, AT).await,
        Err(ComputerControlError::ActionInFlight(_))
    ));

    // A new database connection is the restart boundary: neither the action nor pending takeover is
    // process memory, and recovery must reconcile the exact call before control can move.
    drop(conn);
    let mut restarted = pool.acquire().await.expect("reacquire after restart");
    let recovered = ComputerControlStore::load(&mut restarted, TENANT, TARGET)
        .await
        .expect("recover");
    assert_eq!(recovered.active_tool_call_id.as_deref(), Some(CALL_A));
    assert!(recovered.takeover_pending);
    ComputerControlStore::finish_action(&mut restarted, TENANT, TARGET, CALL_A, 1)
        .await
        .expect("effect settled or reconciled");
    let user = ComputerControlStore::complete_takeover(&mut restarted, TENANT, TARGET, 1, AT)
        .await
        .expect("takeover after drain");
    assert_eq!(user.holder, MachineHolder::User);
    assert_eq!(user.generation, 2);
    assert!(matches!(
        ComputerControlStore::begin_action(&mut restarted, TENANT, TARGET, CALL_B, 2).await,
        Err(ComputerControlError::Held("user"))
    ));
    let agent = ComputerControlStore::handback(&mut restarted, TENANT, TARGET, 2, AT)
        .await
        .expect("explicit handback");
    assert_eq!(agent.holder, MachineHolder::Agent);
    assert_eq!(agent.generation, 3);
    assert!(agent.agent_may_input());
    assert!(matches!(
        ComputerControlStore::begin_action(&mut restarted, TENANT, TARGET, CALL_B, 2).await,
        Err(ComputerControlError::StaleGeneration { .. })
    ));
    ComputerControlStore::begin_action(&mut restarted, TENANT, TARGET, CALL_B, 3)
        .await
        .expect("new generation may act");
    drop(restarted);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_takeover_racing_an_action_has_only_safe_outcomes() {
    let Some((name, pool)) = prepare("computer_race").await else {
        common::blocked_marker();
        return;
    };
    let barrier = Arc::new(Barrier::new(3));
    let action_pool = pool.clone();
    let action_barrier = Arc::clone(&barrier);
    let action = tokio::spawn(async move {
        let mut conn = action_pool.acquire().await.expect("action connection");
        action_barrier.wait().await;
        ComputerControlStore::begin_action(&mut conn, TENANT, TARGET, CALL_A, 1).await
    });
    let takeover_pool = pool.clone();
    let takeover_barrier = Arc::clone(&barrier);
    let takeover = tokio::spawn(async move {
        let mut conn = takeover_pool.acquire().await.expect("takeover connection");
        takeover_barrier.wait().await;
        ComputerControlStore::request_takeover(&mut conn, TENANT, TARGET, 1).await
    });
    barrier.wait().await;
    let action = action.await.expect("action join");
    let takeover = takeover
        .await
        .expect("takeover join")
        .expect("request always fences");
    assert!(takeover.takeover_pending);

    let mut conn = pool.acquire().await.expect("verify connection");
    let state = ComputerControlStore::load(&mut conn, TENANT, TARGET)
        .await
        .expect("load");
    match action {
        Ok(_) => {
            assert_eq!(state.active_tool_call_id.as_deref(), Some(CALL_A));
            assert!(matches!(
                ComputerControlStore::complete_takeover(&mut conn, TENANT, TARGET, 1, AT).await,
                Err(ComputerControlError::ActionInFlight(_))
            ));
            ComputerControlStore::finish_action(&mut conn, TENANT, TARGET, CALL_A, 1)
                .await
                .expect("finish winner");
        }
        Err(ComputerControlError::TakeoverPending) => {
            assert!(state.active_tool_call_id.is_none());
        }
        Err(other) => panic!("unsafe race outcome: {other:?}"),
    }
    let user = ComputerControlStore::complete_takeover(&mut conn, TENANT, TARGET, 1, AT)
        .await
        .expect("complete only after no action");
    assert_eq!(user.holder, MachineHolder::User);
    assert!(user.active_tool_call_id.is_none());
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn exact_tool_call_and_tenant_scope_fail_closed() {
    let Some((name, pool)) = prepare("computer_scope").await else {
        common::blocked_marker();
        return;
    };
    let mut conn = pool.acquire().await.expect("acquire");
    assert!(matches!(
        ComputerControlStore::begin_action(&mut conn, TENANT, TARGET, "not-a-call", 1).await,
        Err(ComputerControlError::ToolCallIdInvalid(_))
    ));
    ComputerControlStore::begin_action(&mut conn, TENANT, TARGET, CALL_A, 1)
        .await
        .expect("begin");
    assert!(matches!(
        ComputerControlStore::finish_action(&mut conn, TENANT, TARGET, CALL_B, 1).await,
        Err(ComputerControlError::ActionMismatch { .. })
    ));
    assert!(matches!(
        ComputerControlStore::load(&mut conn, "tn_01J8Z3K6F1N8VQ2X5W9Y0OTHER", TARGET).await,
        Err(ComputerControlError::NotFound(_))
    ));
    drop(conn);
    common::drop_pool(&pool, &name).await;
}
