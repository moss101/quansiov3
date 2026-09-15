//! OPS-004: usage rebuild, quota race, budget alert. Billing is a projection.

use std::sync::{Arc, Mutex};
use std::thread;

use chrono::{TimeZone, Utc};
use quansio_core::{CorrelationId, Sequence, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventType, RuntimeEvent};
use quansio_server::usage::{
    admit_effect, QuotaLimit, QuotaPolicy, UsageDelta, UsageMeter, UsageProjection, UsageRecord,
};
use serde_json::json;

const TENANT: &str = "tn_01USAGEPROJ00000000000001";
const WORKSPACE: &str = "ws_01USAGEPROJ00000000000001";

fn event(seq: i64, event_type: &str, payload: serde_json::Value) -> RuntimeEvent {
    let mut generator = UlidGenerator::new();
    let draft = EventDraft::new(
        "run",
        "run_01USAGEPROJ00000000000001",
        seq as u64,
        EventType::parse(event_type).expect("type"),
        CorrelationId::generate(&mut generator),
        Actor::system("usage-test"),
    )
    .with_workspace(WORKSPACE)
    .with_payload(payload);
    RuntimeEvent::from_draft(draft, TENANT, Sequence::new(seq).expect("seq"))
}

fn model_usage(seq: i64, input: i64, output: i64, cost: i64) -> RuntimeEvent {
    event(
        seq,
        "model.usage_recorded",
        json!({
            "run_id": "run_01USAGEPROJ00000000000001",
            "input_tokens": input,
            "output_tokens": output,
            "cache_tokens": 0,
            "cost_minor_units": cost,
        }),
    )
}

#[test]
fn usage_rebuild_from_source_events_is_deterministic() {
    let events = vec![
        model_usage(2, 10, 20, 5),
        event(
            1,
            "effect.settled",
            json!({"connector_id": "cnx_github", "run_id": "run_01USAGEPROJ00000000000001"}),
        ),
        event(
            3,
            "artifact.version_added",
            json!({"size_bytes": 2048, "run_id": "run_01USAGEPROJ00000000000001"}),
        ),
        event(4, "run.started", json!({"ignored": true})),
    ];
    let first = UsageProjection::rebuild(&events);
    let reversed: Vec<_> = events.iter().rev().cloned().collect();
    let second = UsageProjection::rebuild(&reversed);
    assert_eq!(first.records().len(), second.records().len());
    let left: Vec<_> = first
        .records()
        .iter()
        .map(|record| {
            (
                record.id.clone(),
                record.meter,
                record.quantity,
                record.source_event_id.clone(),
            )
        })
        .collect();
    let right: Vec<_> = second
        .records()
        .iter()
        .map(|record| {
            (
                record.id.clone(),
                record.meter,
                record.quantity,
                record.source_event_id.clone(),
            )
        })
        .collect();
    assert_eq!(left, right, "rebuild is a pure function of the event set");
    assert!(first.total(UsageMeter::ModelInputTokens) > 0.0);
    assert_eq!(first.total(UsageMeter::ConnectorCalls), 1.0);
    assert_eq!(first.total(UsageMeter::StorageBytes), 2048.0);
    // Same source event + meter always yields the same use_ id.
    let id_a = UsageRecord::identity_for(
        TENANT,
        &events[0].event_id.to_string(),
        UsageMeter::ModelInputTokens,
    );
    let id_b = UsageRecord::identity_for(
        TENANT,
        &events[0].event_id.to_string(),
        UsageMeter::ModelInputTokens,
    );
    assert_eq!(id_a, id_b);
    assert!(id_a.starts_with("use_"));
}

#[test]
fn budget_alert_on_soft_quota_still_admits() {
    let mut projection = UsageProjection::rebuild(&[model_usage(1, 80, 0, 0)]);
    let policy = QuotaPolicy::new().with(QuotaLimit {
        meter: UsageMeter::ModelInputTokens,
        soft: Some(100.0),
        hard: Some(200.0),
    });
    let delta = UsageDelta {
        tenant_id: TENANT.into(),
        workspace_id: Some(WORKSPACE.into()),
        meter: UsageMeter::ModelInputTokens,
        quantity: 30.0,
        source_event_id: "evt_soft".into(),
        occurred_at: Utc.with_ymd_and_hms(2026, 9, 15, 8, 35, 0).unwrap(),
    };
    let mut committed = false;
    let decision = admit_effect(&mut projection, &policy, delta, || {
        committed = true;
        Ok::<(), String>(())
    })
    .expect("soft admits");
    assert!(decision.alerts());
    assert!(decision.admits());
    assert!(committed, "soft quota must still commit the effect");
    assert_eq!(projection.total(UsageMeter::ModelInputTokens), 110.0);
}

#[test]
fn quota_failure_does_not_partially_commit_a_new_effect() {
    let mut projection = UsageProjection::rebuild(&[model_usage(1, 180, 0, 0)]);
    let policy = QuotaPolicy::new().with(QuotaLimit {
        meter: UsageMeter::ModelInputTokens,
        soft: Some(100.0),
        hard: Some(200.0),
    });
    let delta = UsageDelta {
        tenant_id: TENANT.into(),
        workspace_id: Some(WORKSPACE.into()),
        meter: UsageMeter::ModelInputTokens,
        quantity: 50.0,
        source_event_id: "evt_hard".into(),
        occurred_at: Utc.with_ymd_and_hms(2026, 9, 15, 8, 35, 0).unwrap(),
    };
    let mut committed = false;
    let error = admit_effect(&mut projection, &policy, delta, || {
        committed = true;
        Ok::<(), String>(())
    })
    .expect_err("hard deny");
    assert!(
        !committed,
        "quota failure must not reserve/commit an effect"
    );
    assert_eq!(projection.total(UsageMeter::ModelInputTokens), 180.0);
    assert!(error.to_string().contains("quota exceeded"));
}

#[test]
fn quota_race_admits_at_most_one_winner() {
    let projection = Arc::new(Mutex::new(UsageProjection::rebuild(&[model_usage(
        1, 50, 0, 0,
    )])));
    let policy = Arc::new(QuotaPolicy::new().with(QuotaLimit {
        meter: UsageMeter::ModelInputTokens,
        soft: Some(80.0),
        hard: Some(100.0),
    }));
    let committed = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut joins = Vec::new();
    for worker in 0..2u8 {
        let projection = Arc::clone(&projection);
        let policy = Arc::clone(&policy);
        let committed = Arc::clone(&committed);
        joins.push(thread::spawn(move || {
            let delta = UsageDelta {
                tenant_id: TENANT.into(),
                workspace_id: Some(WORKSPACE.into()),
                meter: UsageMeter::ModelInputTokens,
                quantity: 40.0,
                source_event_id: format!("evt_race_{worker}"),
                occurred_at: Utc.with_ymd_and_hms(2026, 9, 15, 8, 35, 0).unwrap(),
            };
            let mut guard = projection.lock().expect("lock");
            admit_effect(&mut guard, &policy, delta, || {
                committed.lock().expect("c").push(worker);
                Ok::<(), String>(())
            })
        }));
    }
    let results: Vec<_> = joins
        .into_iter()
        .map(|join| join.join().expect("thread"))
        .collect();
    let wins = results.iter().filter(|item| item.is_ok()).count();
    let losses = results.iter().filter(|item| item.is_err()).count();
    assert_eq!(wins, 1, "exactly one racer may admit");
    assert_eq!(losses, 1, "the other racer must be quota-denied");
    assert_eq!(committed.lock().expect("c").len(), 1);
    let total = projection
        .lock()
        .expect("lock")
        .total(UsageMeter::ModelInputTokens);
    assert_eq!(total, 90.0);
}
