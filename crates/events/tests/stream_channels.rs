//! Channel and tenant isolation tests (CORE-009, DOMAIN.md §9.3).
//!
//! A subscriber to `workspace:`/`thread:`/`run:`/`target:` receives only its own
//! channel's events, and never another tenant's events even for a channel string that
//! names another tenant's aggregate.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use quansio_events::{
    Channel, EventStore, LiveSendOutcome, StreamConfig, StreamError, StreamSubscription,
};
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, collect_frames, commit_event, drop_pool, fresh_migrated_database,
    scratch_name, seed_tenant, SeedEvent,
};

const TENANT_A: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0S0001";
const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0T0001";
const WORKSPACE_A: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0S0001";
const WORKSPACE_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0T0001";
const RUN_A: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0S0001";
const RUN_B: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0T0001";
const THREAD_A: &str = "thr_01J8Z3K6F1N8VQ2X5W9Y0S0001";
const TARGET_A: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0S0001";
const NODE_A: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0S0001";

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_migrated_database(&name).await?;
    seed_tenant(&pool, TENANT_A, WORKSPACE_A).await;
    seed_tenant(&pool, TENANT_B, WORKSPACE_B).await;
    Some((name, pool))
}

async fn event(
    store: &EventStore,
    tenant: &str,
    workspace: &str,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
) -> String {
    commit_event(
        store,
        tenant,
        SeedEvent {
            aggregate_type,
            aggregate_id,
            aggregate_version: 1,
            event_type,
            workspace_id: Some(workspace),
            payload: serde_json::json!({}),
        },
    )
    .await
    .to_string()
}

/// Deliver every event a subscriber to `channel` receives, then close the session.
async fn drain(store: &EventStore, tenant: &str, channel: Channel) -> Vec<String> {
    let subscription =
        StreamSubscription::new(tenant, "isolation", vec![channel]).expect("subscription");
    let (mut session, live) = store
        .subscribe(subscription, StreamConfig::default())
        .await
        .expect("subscribe");
    drop(live);
    let frames = collect_frames(&mut session).await;
    session.close().await;
    frames
        .iter()
        .filter_map(|frame| frame.event().map(|event| event.event_id.to_string()))
        .collect()
}

#[tokio::test]
async fn a_subscriber_receives_only_its_own_channel_and_tenant() {
    let Some((name, pool)) = prepare("streamchannels").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());

    let node_event = event(
        &store,
        TENANT_A,
        WORKSPACE_A,
        "work",
        NODE_A,
        "work.node_created",
    )
    .await;
    let run_event = event(&store, TENANT_A, WORKSPACE_A, "run", RUN_A, "run.queued").await;
    let thread_event = event(
        &store,
        TENANT_A,
        WORKSPACE_A,
        "thread",
        THREAD_A,
        "thread.message_posted",
    )
    .await;
    let target_event = event(
        &store,
        TENANT_A,
        WORKSPACE_A,
        "target",
        TARGET_A,
        "target.ready",
    )
    .await;
    let other_tenant_event = event(&store, TENANT_B, WORKSPACE_B, "run", RUN_B, "run.queued").await;

    let workspace = drain(
        &store,
        TENANT_A,
        Channel::parse(TENANT_A, &format!("workspace:{WORKSPACE_A}")).expect("channel"),
    )
    .await;
    assert_eq!(
        workspace,
        vec![
            node_event.clone(),
            run_event.clone(),
            thread_event.clone(),
            target_event.clone(),
        ],
        "the workspace channel carries every workspace event, and nothing else"
    );

    for (text, expected) in [
        (format!("run:{RUN_A}"), vec![run_event.clone()]),
        (format!("thread:{THREAD_A}"), vec![thread_event.clone()]),
        (format!("target:{TARGET_A}"), vec![target_event.clone()]),
    ] {
        let delivered = drain(
            &store,
            TENANT_A,
            Channel::parse(TENANT_A, &text).expect("channel"),
        )
        .await;
        assert_eq!(delivered, expected, "channel {text}");
    }

    // A tenant-scoped agent may name another tenant's aggregate, and still sees nothing:
    // reads are scoped to the subscriber's tenant.
    let foreign = drain(
        &store,
        TENANT_A,
        Channel::parse(TENANT_A, &format!("workspace:{WORKSPACE_B}")).expect("channel"),
    )
    .await;
    assert!(foreign.is_empty(), "no other tenant's workspace events");
    let foreign = drain(
        &store,
        TENANT_A,
        Channel::parse(TENANT_A, &format!("run:{RUN_B}")).expect("channel"),
    )
    .await;
    assert!(foreign.is_empty(), "no other tenant's run events");

    // Tenant B sees its own event and none of tenant A's.
    let own = drain(
        &store,
        TENANT_B,
        Channel::parse(TENANT_B, &format!("workspace:{WORKSPACE_B}")).expect("channel"),
    )
    .await;
    assert_eq!(own, vec![other_tenant_event]);
    let foreign = drain(
        &store,
        TENANT_B,
        Channel::parse(TENANT_B, &format!("run:{RUN_A}")).expect("channel"),
    )
    .await;
    assert!(foreign.is_empty());

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn channel_and_subscription_validation_fails_closed() {
    let Some((name, pool)) = prepare("streamvalidate").await else {
        blocked_marker();
        return;
    };

    assert!(
        matches!(
            Channel::parse(TENANT_A, "run:wn_01J8Z3K6F1N8VQ2X5W9Y0S0001"),
            Err(StreamError::MalformedChannel(_))
        ),
        "the id must match the channel kind's prefix"
    );
    assert!(matches!(
        Channel::parse(TENANT_A, "unknown:run_01J8Z3K6F1N8VQ2X5W9Y0S0001"),
        Err(StreamError::MalformedChannel(_))
    ));
    assert!(matches!(
        Channel::parse(TENANT_A, "run"),
        Err(StreamError::MalformedChannel(_))
    ));
    assert!(matches!(
        Channel::parse("not-a-tenant", "run_01J8Z3K6F1N8VQ2X5W9Y0S0001"),
        Err(StreamError::MalformedChannel(_)) | Err(StreamError::InvalidTenantId(_))
    ));
    assert!(matches!(
        StreamSubscription::new(TENANT_A, "consumer", Vec::new()),
        Err(StreamError::EmptySubscription)
    ));

    let other_tenants_channel =
        Channel::parse(TENANT_B, &format!("workspace:{WORKSPACE_B}")).expect("channel");
    assert!(matches!(
        StreamSubscription::new(TENANT_A, "consumer", vec![other_tenants_channel]),
        Err(StreamError::ChannelTenantMismatch { .. })
    ));

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn live_frames_are_transient_droppable_and_never_persisted() {
    let Some((name, pool)) = prepare("streamlive").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    let channel = Channel::parse(TENANT_A, &format!("run:{RUN_A}")).expect("channel");
    let config = StreamConfig {
        live_capacity: 4,
        ..StreamConfig::default()
    };
    let (mut session, live) = store
        .subscribe(
            StreamSubscription::new(TENANT_A, "live-consumer", vec![channel.clone()])
                .expect("subscription"),
            config,
        )
        .await
        .expect("subscribe");
    assert_eq!(live.capacity(), 4);

    for index in 0..4 {
        let outcome = live
            .send(
                &channel,
                serde_json::json!({"kind": "model.delta", "index": index}),
            )
            .expect("subscribed channel");
        assert_eq!(outcome, LiveSendOutcome::Sent);
    }
    let dropped = live
        .send(&channel, serde_json::json!({"kind": "model.delta"}))
        .expect("subscribed channel");
    assert_eq!(
        dropped,
        LiveSendOutcome::Dropped,
        "live frames are dropped, never buffered without bound"
    );

    // A live frame for a channel outside the subscription is refused.
    let other = Channel::parse(TENANT_A, &format!("run:{RUN_B}")).expect("channel");
    assert!(matches!(
        live.send(&other, serde_json::json!({"kind": "model.delta"})),
        Err(StreamError::ChannelNotSubscribed(_))
    ));

    // Live frames are never persisted.
    assert_eq!(
        store
            .read_events_after(TENANT_A, None, 100)
            .await
            .expect("read events")
            .len(),
        0,
        "a live frame is not a RuntimeEvent"
    );

    drop(live);
    let frames = collect_frames(&mut session).await;
    assert_eq!(
        frames.len(),
        4,
        "the buffered live frames are still delivered"
    );
    for frame in &frames {
        assert_eq!(frame.kind(), "live");
        assert!(
            frame.cursor().is_none(),
            "live frames have no durable cursor"
        );
        assert!(frame.event().is_none());
        assert_eq!(frame.to_json_value()["kind"], "live");
        assert_eq!(frame.to_json_value()["channel"], channel.as_str());
    }
    session.close().await;

    drop_pool(&pool, &name).await;
}
