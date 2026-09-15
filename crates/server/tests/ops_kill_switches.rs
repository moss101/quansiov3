//! OPS-008: kill switches are audited/recoverable; effect freeze does not corrupt reads.

use quansio_server::control::ops::{KillSwitch, KillSwitchBoard};

#[test]
fn kill_switch_e2e_is_audited_and_recoverable() {
    let mut board = KillSwitchBoard::default();
    board.engage(KillSwitch::EffectFreeze, "*", "usr_ops");
    board.engage(KillSwitch::ProviderDisable, "anthropic", "usr_ops");
    assert!(board.is_engaged(KillSwitch::EffectFreeze, "*"));
    assert!(board.provider_disabled("anthropic"));
    assert!(board.release(KillSwitch::EffectFreeze, "*", "usr_ops"));
    assert!(!board.is_engaged(KillSwitch::EffectFreeze, "*"));
    let actions: Vec<_> = board
        .audit()
        .iter()
        .map(|row| (row.switch, row.action))
        .collect();
    assert_eq!(
        actions,
        vec![
            (KillSwitch::EffectFreeze, "engaged"),
            (KillSwitch::ProviderDisable, "engaged"),
            (KillSwitch::EffectFreeze, "released"),
        ]
    );
    assert!(
        board.is_engaged(KillSwitch::ProviderDisable, "anthropic"),
        "releasing freeze must not drop an unrelated switch"
    );
}

#[test]
fn worker_quarantine_is_targeted_and_reversible() {
    let mut board = KillSwitchBoard::default();
    board.engage(KillSwitch::WorkerQuarantine, "qworkerd_1", "usr_ops");
    assert!(board.worker_quarantined("qworkerd_1"));
    assert!(!board.worker_quarantined("qworkerd_2"));
    assert!(board.release(KillSwitch::WorkerQuarantine, "qworkerd_1", "usr_ops"));
    assert!(!board.worker_quarantined("qworkerd_1"));
}

#[test]
fn provider_outage_drill_and_effect_freeze() {
    let mut board = KillSwitchBoard::default();
    board.engage(KillSwitch::ProviderDisable, "openai", "usr_ops");
    board.engage(KillSwitch::EffectFreeze, "*", "usr_ops");
    assert!(!board.allows_new_effect(2), "tier ≥ 2 must freeze");
    assert!(!board.allows_new_effect(3));
    assert!(board.allows_new_effect(1), "tier 1 reads may continue");
    board.capture_evidence("evd_during_freeze");
    assert_eq!(board.evidence(), &["evd_during_freeze".to_string()]);
    assert!(
        board.provider_disabled("openai"),
        "provider outage drill disables the named provider"
    );
    assert!(!board.provider_disabled("anthropic"));
    board.release(KillSwitch::EffectFreeze, "*", "usr_ops");
    assert!(board.allows_new_effect(3));
    assert_eq!(
        board.evidence(),
        &["evd_during_freeze".to_string()],
        "freeze must not corrupt captured evidence"
    );
}

#[test]
fn every_dossier_switch_exists() {
    assert_eq!(KillSwitch::ALL.len(), 7);
    let names: Vec<_> = KillSwitch::ALL.iter().map(|item| item.as_str()).collect();
    assert!(names.contains(&"effect.freeze"));
    assert!(names.contains(&"worker.quarantine"));
    assert!(names.contains(&"provider.disable"));
}
