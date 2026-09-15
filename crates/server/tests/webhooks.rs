//! APP-015 webhook tenant scope, redaction, egress and dedupe.

use quansio_server::notify::{already_delivered, may_deliver, redact, WebhookSubscription};
use serde_json::json;

fn sub() -> WebhookSubscription {
    WebhookSubscription {
        id: "whk_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".into(),
        tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".into(),
        workspace_id: Some("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".into()),
        url: "https://example.test/hook".into(),
        event_types: vec!["run.succeeded".into()],
        disabled: false,
    }
}

#[test]
fn delivery_is_tenant_scoped_and_obeys_egress_and_kill_switch() {
    let sub = sub();
    assert!(may_deliver(
        &sub,
        "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        Some("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"),
        "run.succeeded",
        true,
        false,
    ));
    assert!(!may_deliver(
        &sub,
        "tn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
        Some("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"),
        "run.succeeded",
        true,
        false,
    ));
    assert!(!may_deliver(
        &sub,
        "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        Some("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"),
        "run.succeeded",
        false,
        false,
    ));
    assert!(!may_deliver(
        &sub,
        "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        Some("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"),
        "run.succeeded",
        true,
        true,
    ));
}

#[test]
fn consumers_dedupe_on_stable_event_ids() {
    let delivered = vec!["evt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string()];
    assert!(already_delivered(
        &delivered,
        "evt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
    ));
    assert!(!already_delivered(
        &delivered,
        "evt_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
    ));
}

#[test]
fn secrets_are_redacted_from_webhook_payloads() {
    let payload = json!({
        "event_id": "evt_1",
        "api_key": "sk-live",
        "nested": { "password": "hunter2", "ok": true }
    });
    let redacted = redact(&payload);
    assert_eq!(redacted["api_key"], "[redacted]");
    assert_eq!(redacted["nested"]["password"], "[redacted]");
    assert_eq!(redacted["nested"]["ok"], true);
    assert_eq!(redacted["event_id"], "evt_1");
}
