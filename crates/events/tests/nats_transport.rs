//! CORE-003 real boundary: the outbox's NATS JetStream transport against a real server.
//!
//! Environment: `QUANSIO_TEST_NATS_URL` — the dev stack's JetStream server, e.g.
//! `nats://127.0.0.1:54230` (`infra/compose`, `nats -js`). Absent → `BLOCKED_EXTERNAL`
//! marker and return; this suite is never satisfied by a stub.

use quansio_core::UlidGenerator;
use quansio_events::{EventTransport, NatsConfig, NatsTransport, TransportError};
use serde_json::json;

fn nats_url() -> Option<String> {
    std::env::var("QUANSIO_TEST_NATS_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

fn blocked() {
    eprintln!(
        "BLOCKED_EXTERNAL: QUANSIO_TEST_NATS_URL is not set; no JetStream server to publish to"
    );
}

fn unique_subject() -> (String, String) {
    let mut ids = UlidGenerator::new();
    (
        format!("q.tn_nats{}.run.run.started", ids.generate()),
        format!("evt_{}", ids.generate()),
    )
}

#[tokio::test]
async fn jetstream_stores_an_event_once_however_often_it_is_published() {
    let Some(url) = nats_url() else {
        blocked();
        return;
    };
    let transport = NatsTransport::new(NatsConfig::parse(&url).expect("url"));
    let (subject, event_id) = unique_subject();
    let payload = json!({ "event_id": event_id, "type": "run.started" });
    let first = transport
        .publish_acknowledged(&subject, &event_id, &payload)
        .await
        .expect("first publish");
    assert!(!first.duplicate, "a new event is stored");
    // A crash between the acknowledgement and the outbox marking the row published replays it.
    let replay = transport
        .publish_acknowledged(&subject, &event_id, &payload)
        .await
        .expect("replay");
    assert!(
        replay.duplicate,
        "the stream discards the replay by Nats-Msg-Id"
    );
    assert_eq!(replay.sequence, first.sequence, "one stored copy");
    // Another event is another message.
    let (_, other) = unique_subject();
    let next = transport
        .publish_acknowledged(&subject, &other, &json!({ "event_id": other }))
        .await
        .expect("next");
    assert!(!next.duplicate);
    assert!(next.sequence > first.sequence);
    // The EventTransport the outbox calls acknowledges the same way.
    transport
        .publish(&subject, &event_id, &payload)
        .await
        .expect("trait publish");
}

#[tokio::test]
async fn an_unreachable_server_is_unavailable_not_a_silent_success() {
    if nats_url().is_none() {
        blocked();
        return;
    }
    // A port nothing listens on: the publish fails, typed, so the outbox keeps the row.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let transport =
        NatsTransport::new(NatsConfig::parse(&format!("nats://127.0.0.1:{port}")).expect("url"));
    let (subject, event_id) = unique_subject();
    let error = transport
        .publish(&subject, &event_id, &json!({}))
        .await
        .expect_err("nothing listens");
    assert!(matches!(error, TransportError::Unavailable(_)), "{error}");
}

#[tokio::test]
async fn the_transport_reconnects_after_the_connection_is_lost() {
    let Some(url) = nats_url() else {
        blocked();
        return;
    };
    let transport = NatsTransport::new(NatsConfig::parse(&url).expect("url"));
    let (subject, event_id) = unique_subject();
    transport
        .publish(&subject, &event_id, &json!({}))
        .await
        .expect("publish");
    // Publishing to a subject no stream captures is refused (503 no responders) and drops
    // the connection; the next publish reconnects.
    let (_, stray) = unique_subject();
    let refused = transport
        .publish(&format!("unrouted.{stray}"), &stray, &json!({}))
        .await
        .expect_err("no stream captures it");
    assert!(
        matches!(refused, TransportError::Unavailable(_)),
        "{refused}"
    );
    let (_, again) = unique_subject();
    transport
        .publish(&subject, &again, &json!({}))
        .await
        .expect("reconnected");
}
