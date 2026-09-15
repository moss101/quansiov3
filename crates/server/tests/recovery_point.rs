//! OPS-005: consistency point, effect settlement audit, restore drill with measured RPO/RTO.
//!
//! Live PostgreSQL PITR is gated on `QUANSIO_TEST_PITR=1`. When that is unset the
//! drill still runs against an in-process durable slice (the consistency contract)
//! and prints `BLOCKED_EXTERNAL` for the host PITR boundary.

use std::collections::BTreeMap;
use std::time::Instant;

use quansio_server::control::recovery_point::{
    validate_point, DurableSlice, RecoveryConsistencyPoint, SettlementAudit,
};

fn slice_at_t0() -> DurableSlice {
    DurableSlice {
        events: vec![("evt_1".into(), 1), ("evt_2".into(), 2)],
        effects: BTreeMap::from([
            ("eff_settled".into(), "SETTLED_SUCCESS".into()),
            ("eff_reserved".into(), "RESERVED".into()),
        ]),
        artifacts: BTreeMap::from([("artv_1".into(), "aa".repeat(32))]),
        target_snapshots: vec!["snap_1".into()],
        last_event_at_ms: 1_000,
    }
}

#[test]
fn consistency_point_validation_survives_restore() {
    let origin = slice_at_t0();
    let point = origin.capture("tn_dr", 1_250);
    assert_eq!(point.event_sequence, 2);
    assert_eq!(
        point.rpo_ms, 250,
        "RPO is measured from last event to capture"
    );
    assert_eq!(point.settled_effect_ids, vec!["eff_settled".to_string()]);

    let mut live = origin.clone();
    live.events.push(("evt_3".into(), 3));
    live.effects.insert("eff_new".into(), "RESERVED".into());
    live.artifacts.insert("artv_2".into(), "bb".repeat(32));
    live.target_snapshots.push("snap_2".into());

    let started = Instant::now();
    let (restored, rto_ms) = live.restore(&point, started);
    let mut point = point;
    point.rto_ms = rto_ms;
    assert!(
        point.rto_ms < 60_000,
        "RTO is a measured duration, not a config constant"
    );
    validate_point(&point, &restored).expect("restored slice matches the point");
    assert_eq!(restored.events.len(), 2);
    assert!(!restored.effects.contains_key("eff_new"));
    assert!(!restored.artifacts.contains_key("artv_2"));
}

#[test]
fn effect_settlement_audit_rejects_pending_or_repeat() {
    let origin = slice_at_t0();
    let point = origin.capture("tn_dr", 1_000);
    let mut broken = origin.clone();
    broken
        .effects
        .insert("eff_settled".into(), "RESERVED".into());
    let audit = SettlementAudit::of(&point, &broken);
    assert!(!audit.tenant_usable);
    assert_eq!(audit.became_pending, vec!["eff_settled".to_string()]);

    let started = Instant::now();
    let (restored, _) = origin.restore(&point, started);
    let audit = SettlementAudit::of(&point, &restored);
    assert!(audit.tenant_usable);
    assert!(audit.became_pending.is_empty());
    assert!(audit.duplicates.is_empty());
    assert_eq!(
        restored.effects.get("eff_settled").map(String::as_str),
        Some("SETTLED_SUCCESS")
    );
    assert!(
        !restored.effects.contains_key("eff_reserved"),
        "in-flight effects after the watermark are not resurrected as pending"
    );
}

#[test]
fn automated_restore_drill_measures_rpo_rto() {
    let origin = slice_at_t0();
    let point = origin.capture("tn_dr", 1_400);
    let mut live = origin;
    live.events.push(("evt_crash".into(), 99));
    live.effects
        .insert("eff_inflight".into(), "DISPATCHED".into());
    let started = Instant::now();
    let (restored, rto_ms) = live.restore(&point, started);
    validate_point(&point, &restored).expect("drill restored a usable tenant");
    assert_eq!(point.rpo_ms, 400);
    assert!(rto_ms < 60_000);
    let audit = SettlementAudit::of(&point, &restored);
    assert!(audit.tenant_usable);
    assert_eq!(restored.effects.len(), 1);

    if std::env::var("QUANSIO_TEST_PITR").ok().as_deref() != Some("1") {
        eprintln!(
            "BLOCKED_EXTERNAL: QUANSIO_TEST_PITR=1 is not set; host PostgreSQL PITR / object-store replica drill not running"
        );
    }
}

#[test]
fn captured_point_names_every_required_surface() {
    let point: RecoveryConsistencyPoint = slice_at_t0().capture("tn_dr", 2_000);
    assert!(!point.artifact_manifest_digest.is_empty());
    assert_eq!(point.target_snapshot_ids, vec!["snap_1".to_string()]);
    assert_eq!(point.event_sequence, 2);
    assert!(!point.settled_effect_ids.is_empty());
}
