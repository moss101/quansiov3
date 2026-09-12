//! Resumable client streaming tests (CORE-009, DOMAIN.md §9.3).
//!
//! A client that reconnects with the last cursor it received must get exactly the
//! events it missed, in order; a client that presents no cursor resumes from the
//! server-persisted position for its consumer identity.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use quansio_core::Cursor;
use quansio_events::{Channel, EventStore, StreamConfig, StreamFrame, StreamSubscription};
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, collect_frames, commit_event, drop_pool, fresh_migrated_database,
    scratch_name, seed_tenant, SeedEvent,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0C0001";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0C0001";
const RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0C0001";

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_migrated_database(&name).await?;
    seed_tenant(&pool, TENANT, WORKSPACE).await;
    Some((name, pool))
}

async fn commit_run(store: &EventStore, version: u64) -> String {
    commit_event(
        store,
        TENANT,
        SeedEvent {
            aggregate_type: "run",
            aggregate_id: RUN,
            aggregate_version: version,
            event_type: "run.queued",
            workspace_id: Some(WORKSPACE),
            payload: serde_json::json!({"run_id": RUN}),
        },
    )
    .await
    .to_string()
}

fn event_ids(frames: &[StreamFrame]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| frame.event().map(|event| event.event_id.to_string()))
        .collect()
}

fn run_channel() -> Channel {
    Channel::parse(TENANT, &format!("run:{RUN}")).expect("canonical channel")
}

fn subscription(consumer: &str) -> StreamSubscription {
    StreamSubscription::new(TENANT, consumer, vec![run_channel()]).expect("subscription")
}

#[tokio::test]
async fn reconnecting_with_the_last_cursor_replays_exactly_the_missed_events() {
    let Some((name, pool)) = prepare("streamresume").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    let mut first_ids = Vec::new();
    for version in 1..=5 {
        first_ids.push(commit_run(&store, version).await);
    }

    let config = StreamConfig {
        batch_size: 2,
        ..StreamConfig::default()
    };
    let (mut session, live) = store
        .subscribe(subscription("client-1"), config)
        .await
        .expect("subscribe");
    drop(live);
    let frames = collect_frames(&mut session).await;
    assert_eq!(
        event_ids(&frames),
        first_ids,
        "the live stream carries every event in order"
    );
    assert!(frames
        .iter()
        .all(|frame| frame.kind() == "event" && frame.to_json_value()["kind"] == "event"));
    for frame in &frames {
        let cursor = Cursor::decode(frame.cursor().expect("durable frame has a cursor"))
            .expect("cursor decodes");
        assert_eq!(cursor.stream_id(), session.stream_id());
        assert_eq!(
            cursor.sequence(),
            frame.event().expect("event frame").sequence,
            "the cursor names the frame's own position"
        );
    }
    let last_cursor = frames
        .last()
        .expect("frames")
        .cursor()
        .expect("cursor")
        .to_string();
    let second_cursor = frames[1].cursor().expect("cursor").to_string();
    session.close().await;

    // The client is away while two more events commit.
    let mut missed_ids = Vec::new();
    for version in 6..=7 {
        missed_ids.push(commit_run(&store, version).await);
    }

    let (mut session, live) = store
        .subscribe(subscription("client-1").with_cursor(last_cursor), config)
        .await
        .expect("resume");
    drop(live);
    let resumed = collect_frames(&mut session).await;
    assert_eq!(
        event_ids(&resumed),
        missed_ids,
        "exactly the missed events, in order"
    );
    assert!(
        resumed
            .iter()
            .filter_map(StreamFrame::event)
            .map(|event| event.sequence.get())
            .is_sorted(),
        "no reordering"
    );
    session.close().await;

    // Replaying from an earlier cursor loses nothing and admits nothing: events 3..7.
    let (mut session, live) = store
        .subscribe(subscription("client-1").with_cursor(second_cursor), config)
        .await
        .expect("replay");
    drop(live);
    let replayed = collect_frames(&mut session).await;
    assert_eq!(
        event_ids(&replayed),
        first_ids[2..]
            .iter()
            .chain(missed_ids.iter())
            .cloned()
            .collect::<Vec<_>>()
    );
    session.close().await;

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_client_without_a_cursor_resumes_from_the_persisted_consumer_position() {
    let Some((name, pool)) = prepare("streamservercursor").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    let mut first_ids = Vec::new();
    for version in 1..=3 {
        first_ids.push(commit_run(&store, version).await);
    }

    let (mut session, live) = store
        .subscribe(subscription("client-2"), StreamConfig::default())
        .await
        .expect("subscribe");
    drop(live);
    let frames = collect_frames(&mut session).await;
    assert_eq!(event_ids(&frames), first_ids);
    // `close` waits for the reader task, so its checkpoint write has completed.
    session.close().await;

    let mut missed_ids = Vec::new();
    for version in 4..=5 {
        missed_ids.push(commit_run(&store, version).await);
    }

    let (mut session, live) = store
        .subscribe(subscription("client-2"), StreamConfig::default())
        .await
        .expect("resume");
    drop(live);
    let resumed = collect_frames(&mut session).await;
    assert_eq!(
        event_ids(&resumed),
        missed_ids,
        "the server-persisted position resumed the stream"
    );
    session.close().await;

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_cursor_from_another_stream_is_rejected() {
    let Some((name, pool)) = prepare("streammismatch").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    commit_run(&store, 1).await;
    let foreign = Cursor::new(
        format!("workspace:{WORKSPACE}"),
        quansio_core::Sequence::new(1).expect("valid"),
    )
    .encode();

    let error = store
        .subscribe(
            subscription("client-3").with_cursor(foreign),
            StreamConfig::default(),
        )
        .await
        .expect_err("stream mismatch");
    assert!(matches!(
        error,
        quansio_events::StreamError::Store(quansio_events::EventError::CursorStreamMismatch { .. })
    ));

    drop_pool(&pool, &name).await;
}
