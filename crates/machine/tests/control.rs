//! Execution-target and lease lifecycle tests (EXEC-001, DOMAIN.md §8.1–§8.3).
//!
//! The two acceptance statements, tested where they are enforced:
//!
//! * **only machine-control mutates target/lease lifecycle** — asserted structurally, by scanning the
//!   workspace's sources for SQL that writes either table and requiring that only this crate does;
//! * **lease expiry or generation change fences old controllers** — a stale generation is refused a
//!   transition, a second acquirer is refused a live lease, an expired lease fails the check a worker
//!   applies before it acts, and expiring leases bumps the target's generation so the holder's next
//!   attempt is refused for a reason it cannot argue with.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent → `BLOCKED_EXTERNAL`.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use sqlx::PgPool;

use quansio_machine::control::{
    ExecutionTarget, LeaseRequest, LeaseStatus, MachineControl, MachineError, NewTarget, Substrate,
    TargetClass, TargetFence, TargetHealth, TargetStatus,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const NOW: &str = "2026-09-13T10:00:00Z";

struct Fixture {
    name: String,
    pool: PgPool,
    target_id: String,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    seed_tenant(&pool, TENANT, USER, WORKSPACE).await;

    let mut generator = UlidGenerator::new();
    let target_id = CanonicalId::generate(Prefix::ExecutionTarget, &mut generator).to_string();
    let mut conn = pool.acquire().await.expect("acquire");
    MachineControl::register(
        &mut conn,
        TENANT,
        &NewTarget {
            id: target_id.clone(),
            workspace_id: WORKSPACE.to_string(),
            class: TargetClass::PersistentWorkspaceComputer,
            substrate: Substrate::CloudMicrovm,
            image_digest: Some("sha256:fixture".to_string()),
            desired_state: Some("READY".to_string()),
        },
    )
    .await
    .expect("register");
    drop(conn);

    Some(Fixture {
        name,
        pool,
        target_id,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

fn new_id(prefix: Prefix) -> String {
    let mut generator = UlidGenerator::new();
    CanonicalId::generate(prefix, &mut generator).to_string()
}

/// Walk a target to `READY` through the lifecycle rather than writing the status directly: the path a
/// real controller takes is the path under test.
async fn ready(fixture: &Fixture) -> ExecutionTarget {
    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let fence = TargetFence { generation: 1 };
    MachineControl::transition(
        &mut conn,
        TENANT,
        &fixture.target_id,
        &fence,
        TargetStatus::Provisioning,
    )
    .await
    .expect("provisioning");
    MachineControl::transition(
        &mut conn,
        TENANT,
        &fixture.target_id,
        &fence,
        TargetStatus::Ready,
    )
    .await
    .expect("ready")
}

// ------------------------------------------------------------------ desired/observed

#[test]
fn health_is_classified_from_what_the_platform_observes() {
    // No heartbeat: nothing is known, whatever the states say.
    assert_eq!(
        ExecutionTarget::classify_health(None, false, Some("READY"), Some("READY")),
        TargetHealth::Unknown
    );
    // A heartbeat older than the window is stale, and staleness wins over the states: a target that is
    // not beating is not healthy because it once said the right thing.
    assert_eq!(
        ExecutionTarget::classify_health(Some(NOW), true, Some("READY"), Some("BUSY")),
        TargetHealth::Stale
    );
    // Beating and agreeing is healthy; beating and disagreeing has diverged.
    assert_eq!(
        ExecutionTarget::classify_health(Some(NOW), false, Some("READY"), Some("READY")),
        TargetHealth::Healthy
    );
    assert_eq!(
        ExecutionTarget::classify_health(Some(NOW), false, Some("READY"), Some("BUSY")),
        TargetHealth::Diverged
    );
    assert_eq!(
        ExecutionTarget::classify_health(Some(NOW), false, None, None),
        TargetHealth::Healthy,
        "with nothing asked for, a beating target is healthy"
    );
    assert_eq!(TargetHealth::Stale.as_str(), "stale");
}

#[tokio::test]
async fn a_target_reports_what_it_observes_and_the_platform_classifies_it() {
    let Some(fixture) = prepare("machine_health").await else {
        blocked_marker();
        return;
    };
    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let registered = MachineControl::load(&mut conn, TENANT, &fixture.target_id)
        .await
        .expect("load");
    assert_eq!(registered.desired_state.as_deref(), Some("READY"));
    assert_eq!(registered.observed_state, None);
    assert_eq!(
        registered.health(false),
        TargetHealth::Unknown,
        "a target that has never beaten is not healthy"
    );

    let observed = MachineControl::observe(&mut conn, TENANT, &fixture.target_id, "BOOTING", NOW)
        .await
        .expect("observe");
    assert_eq!(observed.observed_state.as_deref(), Some("BOOTING"));
    assert_eq!(observed.last_heartbeat_at.as_deref(), Some(NOW));
    assert_eq!(
        observed.health(false),
        TargetHealth::Diverged,
        "it is not what was asked for"
    );
    assert_eq!(
        observed.health(true),
        TargetHealth::Stale,
        "staleness wins over agreement"
    );

    let settled = MachineControl::observe(&mut conn, TENANT, &fixture.target_id, "READY", NOW)
        .await
        .expect("observe");
    assert_eq!(settled.health(false), TargetHealth::Healthy);

    let desired =
        MachineControl::set_desired_state(&mut conn, TENANT, &fixture.target_id, "DRAINING")
            .await
            .expect("desired");
    assert_eq!(desired.desired_state.as_deref(), Some("DRAINING"));
    assert_eq!(desired.health(false), TargetHealth::Diverged);
    drop(conn);
    finish(fixture).await;
}

// ------------------------------------------------------------------- the lifecycle

#[tokio::test]
async fn a_target_walks_the_lifecycle_domain_8_2_draws() {
    let Some(fixture) = prepare("machine_lifecycle").await else {
        blocked_marker();
        return;
    };

    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let fence = TargetFence { generation: 1 };
    assert_eq!(
        MachineControl::load(&mut conn, TENANT, &fixture.target_id)
            .await
            .expect("load")
            .status,
        TargetStatus::Requested
    );

    let ready = ready(&fixture).await;
    assert_eq!(ready.status, TargetStatus::Ready);
    assert_eq!(
        ready.generation, 1,
        "an ordinary transition does not move the generation"
    );
    assert_eq!(ready.class, TargetClass::PersistentWorkspaceComputer);
    assert_eq!(ready.substrate.as_str(), "cloud_microvm");
    assert_eq!(ready.image_digest.as_deref(), Some("sha256:fixture"));

    for expected in [
        TargetStatus::Busy,
        TargetStatus::Ready,
        TargetStatus::Draining,
        TargetStatus::Stopped,
        TargetStatus::Snapshotted,
        TargetStatus::Destroyed,
    ] {
        let moved =
            MachineControl::transition(&mut conn, TENANT, &fixture.target_id, &fence, expected)
                .await
                .unwrap_or_else(|error| panic!("{expected:?}: {error}"));
        assert_eq!(moved.status, expected);
    }
    drop(conn);
    finish(fixture).await;
}

#[tokio::test]
async fn an_edge_the_lifecycle_does_not_draw_is_refused_with_both_ends() {
    let Some(fixture) = prepare("machine_illegal").await else {
        blocked_marker();
        return;
    };

    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let fence = TargetFence { generation: 1 };
    // REQUESTED → READY is not an edge: a target is provisioned first.
    match MachineControl::transition(
        &mut conn,
        TENANT,
        &fixture.target_id,
        &fence,
        TargetStatus::Ready,
    )
    .await
    {
        Err(MachineError::IllegalTransition { from, to, .. }) => {
            assert_eq!((from, to), ("REQUESTED", "READY"));
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(
        MachineControl::load(&mut conn, TENANT, &fixture.target_id)
            .await
            .expect("load")
            .status,
        TargetStatus::Requested,
        "a refused transition writes nothing"
    );

    // Any live state may fail, and a failure is replaced rather than restarted in place.
    let failed = MachineControl::transition(
        &mut conn,
        TENANT,
        &fixture.target_id,
        &fence,
        TargetStatus::Failed,
    )
    .await
    .expect("fail");
    assert_eq!(failed.status, TargetStatus::Failed);
    let replacing = MachineControl::transition(
        &mut conn,
        TENANT,
        &fixture.target_id,
        &fence,
        TargetStatus::Replacing,
    )
    .await
    .expect("replace");
    assert_eq!(
        replacing.generation, 2,
        "a replacement hands the target to a new controller, so the generation moves"
    );
    drop(conn);
    finish(fixture).await;
}

#[tokio::test]
async fn destroyed_is_terminal() {
    let Some(fixture) = prepare("machine_terminal").await else {
        blocked_marker();
        return;
    };
    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let fence = TargetFence { generation: 1 };
    for status in [
        TargetStatus::Provisioning,
        TargetStatus::Ready,
        TargetStatus::Draining,
        TargetStatus::Stopped,
        TargetStatus::Destroyed,
    ] {
        MachineControl::transition(&mut conn, TENANT, &fixture.target_id, &fence, status)
            .await
            .expect("transition");
    }
    for status in [
        TargetStatus::Ready,
        TargetStatus::Failed,
        TargetStatus::Destroyed,
    ] {
        assert!(
            MachineControl::transition(&mut conn, TENANT, &fixture.target_id, &fence, status)
                .await
                .is_err(),
            "{status:?} must not be reachable from DESTROYED"
        );
    }
    drop(conn);
    finish(fixture).await;
}

// ------------------------------------------------------------------- generation fence

#[tokio::test]
async fn a_stale_generation_cannot_steer_the_target_it_lost() {
    let Some(fixture) = prepare("machine_fence").await else {
        blocked_marker();
        return;
    };
    let target = ready(&fixture).await;
    let stale = TargetFence {
        generation: target.generation - 1,
    };

    let mut conn = fixture.pool.acquire().await.expect("acquire");
    match MachineControl::transition(
        &mut conn,
        TENANT,
        &fixture.target_id,
        &stale,
        TargetStatus::Busy,
    )
    .await
    {
        Err(MachineError::StaleGeneration { current, held, .. }) => {
            assert_eq!((current, held), (1, 0));
        }
        other => panic!("unexpected result: {other:?}"),
    }
    match MachineControl::acquire_lease(
        &mut conn,
        TENANT,
        &LeaseRequest {
            lease_id: &new_id(Prefix::Lease),
            target_id: &fixture.target_id,
            controller_id: "ctl_stale",
            fence: &stale,
            ttl_seconds: 60,
            now: NOW,
        },
    )
    .await
    {
        Err(MachineError::StaleGeneration { .. }) => {}
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(
        MachineControl::load(&mut conn, TENANT, &fixture.target_id)
            .await
            .expect("load")
            .status,
        TargetStatus::Ready,
        "a fenced-out caller writes nothing"
    );
    drop(conn);
    finish(fixture).await;
}

// ---------------------------------------------------------------------- lease races

#[tokio::test]
async fn two_racing_acquirers_cannot_both_hold_a_lease() {
    let Some(fixture) = prepare("machine_race").await else {
        blocked_marker();
        return;
    };
    ready(&fixture).await;
    let first = new_id(Prefix::Lease);
    let second = new_id(Prefix::Lease);

    // Two controllers acquire at the same instant on separate connections. The target row is locked
    // first, so exactly one of them wins and the other is told who holds it.
    let one = async {
        let mut conn = fixture.pool.acquire().await.expect("acquire");
        MachineControl::acquire_lease(
            &mut conn,
            TENANT,
            &LeaseRequest {
                lease_id: &first,
                target_id: &fixture.target_id,
                controller_id: "ctl_one",
                fence: &TargetFence { generation: 1 },
                ttl_seconds: 60,
                now: NOW,
            },
        )
        .await
        .map(|_| first.clone())
    };
    let two = async {
        let mut conn = fixture.pool.acquire().await.expect("acquire");
        MachineControl::acquire_lease(
            &mut conn,
            TENANT,
            &LeaseRequest {
                lease_id: &second,
                target_id: &fixture.target_id,
                controller_id: "ctl_two",
                fence: &TargetFence { generation: 1 },
                ttl_seconds: 60,
                now: NOW,
            },
        )
        .await
        .map(|_| second.clone())
    };
    let (one, two) = tokio::join!(one, two);
    let winners = [&one, &two].iter().filter(|result| result.is_ok()).count();
    assert_eq!(
        winners, 1,
        "exactly one acquirer may hold the target: {one:?} / {two:?}"
    );

    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let leased = MachineControl::load(&mut conn, TENANT, &fixture.target_id)
        .await
        .expect("load");
    let lease_id = leased
        .lease_id
        .expect("the winner is attached to the target");
    let holder = MachineControl::validate_lease(&mut conn, TENANT, &lease_id, 1, NOW)
        .await
        .expect("the winner's lease is live");
    assert!(holder.holder_controller_id == "ctl_one" || holder.holder_controller_id == "ctl_two");

    // Releasing it lets the next controller in.
    MachineControl::release_lease(&mut conn, TENANT, &lease_id)
        .await
        .expect("release");
    let loser = if one.is_ok() { &second } else { &first };
    MachineControl::acquire_lease(
        &mut conn,
        TENANT,
        &LeaseRequest {
            lease_id: loser,
            target_id: &fixture.target_id,
            controller_id: "ctl_loser",
            fence: &TargetFence { generation: 1 },
            ttl_seconds: 60,
            now: NOW,
        },
    )
    .await
    .expect("the released target can be leased again");
    drop(conn);
    finish(fixture).await;
}

// ---------------------------------------------------------------------- lease fence

#[tokio::test]
async fn a_lease_that_is_not_live_does_not_permit_the_action() {
    let Some(fixture) = prepare("machine_lease").await else {
        blocked_marker();
        return;
    };
    ready(&fixture).await;
    let lease_id = new_id(Prefix::Lease);
    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let lease = MachineControl::acquire_lease(
        &mut conn,
        TENANT,
        &LeaseRequest {
            lease_id: &lease_id,
            target_id: &fixture.target_id,
            controller_id: "ctl_one",
            fence: &TargetFence { generation: 1 },
            ttl_seconds: 60,
            now: NOW,
        },
    )
    .await
    .expect("acquire");
    assert_eq!(lease.status, LeaseStatus::Held);

    // The live lease passes, at the generation it was granted for.
    MachineControl::validate_lease(&mut conn, TENANT, &lease_id, 1, NOW)
        .await
        .expect("live lease validates");

    // The wrong generation does not.
    match MachineControl::validate_lease(&mut conn, TENANT, &lease_id, 2, NOW).await {
        Err(MachineError::LeaseFenced { reason, .. }) => {
            assert!(reason.contains("another generation"))
        }
        other => panic!("unexpected result: {other:?}"),
    }
    // Neither does a lease at an instant past its expiry, nor one that was released.
    match MachineControl::validate_lease(&mut conn, TENANT, &lease_id, 1, "2099-01-01T00:00:00Z")
        .await
    {
        Err(MachineError::LeaseFenced { reason, .. }) => assert!(reason.contains("expired")),
        other => panic!("unexpected result: {other:?}"),
    }
    MachineControl::release_lease(&mut conn, TENANT, &lease_id)
        .await
        .expect("release");
    match MachineControl::validate_lease(&mut conn, TENANT, &lease_id, 1, NOW).await {
        Err(MachineError::LeaseFenced { reason, .. }) => assert!(reason.contains("not held")),
        other => panic!("unexpected result: {other:?}"),
    }
    // A renewal after release is refused too: a released lease is not revived by renewing it.
    assert!(
        MachineControl::renew_lease(&mut conn, TENANT, &lease_id, 1, 60, NOW)
            .await
            .is_err()
    );
    drop(conn);
    finish(fixture).await;
}

#[tokio::test]
async fn a_lease_expiring_fences_the_controller_that_held_it() {
    let Some(fixture) = prepare("machine_expiry").await else {
        blocked_marker();
        return;
    };
    ready(&fixture).await;
    let lease_id = new_id(Prefix::Lease);
    let mut conn = fixture.pool.acquire().await.expect("acquire");
    let lease = MachineControl::acquire_lease(
        &mut conn,
        TENANT,
        &LeaseRequest {
            lease_id: &lease_id,
            target_id: &fixture.target_id,
            controller_id: "ctl_one",
            fence: &TargetFence { generation: 1 },
            ttl_seconds: 60,
            now: NOW,
        },
    )
    .await
    .expect("acquire");
    assert_eq!(
        lease.expires_at, "2026-09-13T10:01:00Z",
        "the store renders the instant it stored"
    );

    // A minute later the lease has lapsed: expiring it bumps the target's generation, so the holder's
    // generation no longer matches anything.
    let expired = MachineControl::expire_leases(&mut conn, TENANT, "2026-09-13T10:02:00Z")
        .await
        .expect("expire");
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].status, LeaseStatus::Expired);
    assert_eq!(expired[0].target_id, fixture.target_id);

    assert_eq!(
        MachineControl::load(&mut conn, TENANT, &fixture.target_id)
            .await
            .expect("load")
            .generation,
        2,
        "expiry fences by moving the generation, so the old holder cannot argue its way back in"
    );
    match MachineControl::validate_lease(&mut conn, TENANT, &lease_id, 1, "2026-09-13T10:02:00Z")
        .await
    {
        Err(MachineError::LeaseFenced { .. }) => {}
        other => panic!("unexpected result: {other:?}"),
    }
    match MachineControl::transition(
        &mut conn,
        TENANT,
        &fixture.target_id,
        &TargetFence { generation: 1 },
        TargetStatus::Busy,
    )
    .await
    {
        Err(MachineError::StaleGeneration { current, held, .. }) => {
            assert_eq!((current, held), (2, 1))
        }
        other => panic!("unexpected result: {other:?}"),
    }
    // A revocation fences the same way.
    let second = new_id(Prefix::Lease);
    let renewed = MachineControl::acquire_lease(
        &mut conn,
        TENANT,
        &LeaseRequest {
            lease_id: &second,
            target_id: &fixture.target_id,
            controller_id: "ctl_two",
            fence: &TargetFence { generation: 2 },
            ttl_seconds: 60,
            now: "2026-09-13T10:02:00Z",
        },
    )
    .await
    .expect("the fenced target can be leased by the current controller");
    MachineControl::revoke_lease(&mut conn, TENANT, &renewed.id)
        .await
        .expect("revoke");
    assert_eq!(
        MachineControl::load(&mut conn, TENANT, &fixture.target_id)
            .await
            .expect("load")
            .generation,
        3
    );
    drop(conn);
    finish(fixture).await;
}

// ------------------------------------------------------------------ target readiness

#[tokio::test]
async fn only_a_ready_target_can_be_leased() {
    let Some(fixture) = prepare("machine_ready").await else {
        blocked_marker();
        return;
    };
    let mut conn = fixture.pool.acquire().await.expect("acquire");
    match MachineControl::acquire_lease(
        &mut conn,
        TENANT,
        &LeaseRequest {
            lease_id: &new_id(Prefix::Lease),
            target_id: &fixture.target_id,
            controller_id: "ctl_one",
            fence: &TargetFence { generation: 1 },
            ttl_seconds: 60,
            now: NOW,
        },
    )
    .await
    {
        Err(MachineError::TargetNotReady { status, .. }) => assert_eq!(status, "REQUESTED"),
        other => panic!("unexpected result: {other:?}"),
    }
    drop(conn);
    finish(fixture).await;
}

// ------------------------------------------------------- one authority, and isolation

#[test]
fn only_machine_control_writes_the_target_and_lease_tables() {
    // Acceptance 1. A second module writing these tables would be a second machine-control authority,
    // which is exactly what the acceptance statement forbids — so the check is a source scan over the
    // workspace, not a promise in a doc comment.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut offenders = Vec::new();
    let mut scanned = 0;
    let mut stack = vec![root.join("crates")];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip build output; the owner is `crates/machine/src/control/`.
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if relative.starts_with("crates/machine/src/control/") || relative.contains("/tests/") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            scanned += 1;
            for statement in [
                "INSERT INTO execution_targets",
                "UPDATE execution_targets",
                "DELETE FROM execution_targets",
                "INSERT INTO leases",
                "UPDATE leases",
                "DELETE FROM leases",
            ] {
                if text.contains(statement) {
                    offenders.push(format!("{relative}: {statement}"));
                }
            }
        }
    }
    assert!(
        scanned > 20,
        "the scan must actually walk the workspace (saw {scanned} files)"
    );
    assert!(
        offenders.is_empty(),
        "only machine control may mutate target/lease lifecycle: {offenders:?}"
    );
}

#[tokio::test]
async fn a_tenant_cannot_see_another_tenants_target() {
    let Some(fixture) = prepare("machine_isolation").await else {
        blocked_marker();
        return;
    };
    let other_tenant = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
    let other_workspace = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
    let other_user = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
    seed_tenant(&fixture.pool, other_tenant, other_user, other_workspace).await;

    let mut conn = fixture.pool.acquire().await.expect("acquire");
    match MachineControl::load(&mut conn, other_tenant, &fixture.target_id).await {
        Err(MachineError::TargetNotFound(id)) => assert_eq!(id, fixture.target_id),
        other => panic!("unexpected result: {other:?}"),
    }
    assert!(
        MachineControl::list_for_workspace(&mut conn, other_tenant, WORKSPACE)
            .await
            .expect("list")
            .is_empty(),
        "another tenant's workspace shows nothing"
    );
    drop(conn);
    finish(fixture).await;
}
