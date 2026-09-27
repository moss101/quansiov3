//! The effect taxonomy and the Effect Ledger's value types (DOMAIN.md §7.1, §7.2).
//!
//! The taxonomy fails closed: a document with the wrong version, no classes, an unknown or
//! missing DOMAIN class, a class registered twice, a class without a default decision, an
//! out-of-range tier or an unreadable strategy is refused whole, never applied in part. The
//! status, target and outcome vocabularies round-trip their wire names, and only an unknown
//! outcome is unsettled.

use std::path::Path;

use quansio_server::effects::{
    EffectOutcome, EffectStatus, EffectTaxonomy, OutcomeKind, ReconciliationEvidence,
    ReconciliationStrategy, TargetKind, DOMAIN_EFFECT_CLASSES,
};

fn shipped() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/effects.yaml"))
        .expect("config/effects.yaml")
}

fn refused(source: &str) -> String {
    let error = EffectTaxonomy::from_yaml(source).expect_err("refused");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION", "{error}");
    error.to_string()
}

#[test]
fn the_shipped_taxonomy_registers_every_domain_class_once() {
    let taxonomy = EffectTaxonomy::from_yaml(&shipped()).expect("shipped taxonomy");
    assert_eq!(taxonomy.len(), DOMAIN_EFFECT_CLASSES.len());
    assert!(!taxonomy.is_empty());
    let classes: Vec<String> = taxonomy
        .entries()
        .map(|entry| entry.effect_class.to_string())
        .collect();
    let mut sorted = classes.clone();
    sorted.sort();
    assert_eq!(classes, sorted, "entries are ordered by class");
    let from_file = EffectTaxonomy::from_config_file(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/effects.yaml"),
    )
    .expect("from file");
    assert_eq!(from_file.len(), taxonomy.len());
}

#[test]
fn a_malformed_taxonomy_is_refused_whole() {
    let source = shipped();
    assert!(refused(&source.replacen("version: 1", "version: 2", 1)).contains("version 2"));
    assert!(refused("version: 1\neffect_classes: []\n").contains("no effect classes"));
    refused("version: ["); // not YAML at all
                           // An unknown class is refused by name.
    let unknown = source.replacen(
        "effect_class: read.internal",
        "effect_class: read.everything",
        1,
    );
    let error = EffectTaxonomy::from_yaml(&unknown).expect_err("unknown class");
    assert!(error.to_string().contains("read.everything"), "{error}");
    // Dropping a class leaves a DOMAIN class missing.
    let without_first: String = {
        let start = source
            .find("  - effect_class: read.internal")
            .expect("first row");
        let end = source[start + 1..]
            .find("  - effect_class:")
            .expect("second row")
            + start
            + 1;
        format!("{}{}", &source[..start], &source[end..])
    };
    assert!(refused(&without_first).contains("missing DOMAIN §7.1 classes: read.internal"));
    // Registering a class twice is refused.
    let twice = format!(
        "{source}  - effect_class: read.internal\n    tier: 0\n    default_decision: allow\n    reconciliation: none\n"
    );
    assert!(refused(&twice).contains("registered twice"));
    // A class with no default decision is refused.
    let undecided = source.replacen("default_decision: allow", "default_decision: \" \"", 1);
    assert!(refused(&undecided).contains("no default decision"));
}

#[test]
fn a_taxonomy_with_an_unknown_strategy_or_tier_is_refused() {
    let source = shipped();
    let strategy = source.replacen("reconciliation: none", "reconciliation: hope", 1);
    let error = EffectTaxonomy::from_yaml(&strategy).expect_err("strategy");
    assert!(error.to_string().contains("hope"), "{error}");
    let tier = source.replacen("tier: 0", "tier: 7", 1);
    assert!(
        EffectTaxonomy::from_yaml(&tier).is_err(),
        "a tier above 4 is refused"
    );
}

#[test]
fn an_unreadable_taxonomy_file_is_refused_with_its_path() {
    let error = EffectTaxonomy::from_config_file(Path::new("/nonexistent/effects.yaml"))
        .expect_err("no file");
    assert!(
        error.to_string().contains("/nonexistent/effects.yaml"),
        "{error}"
    );
}

#[test]
fn every_status_round_trips_and_names_its_event() {
    let all = [
        EffectStatus::Proposed,
        EffectStatus::Authorized,
        EffectStatus::Reserved,
        EffectStatus::Dispatched,
        EffectStatus::SettledSuccess,
        EffectStatus::SettledFailed,
        EffectStatus::OutcomeUnknown,
        EffectStatus::Reconciling,
        EffectStatus::ReconciledSuccess,
        EffectStatus::ReconciledFailed,
        EffectStatus::ReconciliationManual,
        EffectStatus::Denied,
        EffectStatus::Expired,
        EffectStatus::Cancelled,
    ];
    for status in all {
        assert_eq!(EffectStatus::parse(status.as_str()).expect("parse"), status);
        assert_eq!(
            status.event_name(),
            format!("effect.{}", status.as_str().to_lowercase())
        );
    }
    let error = EffectStatus::parse("MAYBE").expect_err("unknown");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    // Only these may never be retried blind; only these block a second reservation.
    let unsettled: Vec<_> = all.iter().filter(|status| status.is_unsettled()).collect();
    assert_eq!(
        unsettled,
        [
            &EffectStatus::Dispatched,
            &EffectStatus::OutcomeUnknown,
            &EffectStatus::Reconciling
        ]
    );
    let in_flight: Vec<_> = all.iter().filter(|status| status.is_in_flight()).collect();
    assert_eq!(
        in_flight,
        [
            &EffectStatus::Reserved,
            &EffectStatus::Dispatched,
            &EffectStatus::OutcomeUnknown,
            &EffectStatus::Reconciling
        ]
    );
    assert!(all
        .iter()
        .filter(|status| status.is_terminal())
        .all(|status| !status.is_in_flight()));
}

#[test]
fn target_kinds_outcomes_and_strategies_have_wire_names() {
    for kind in [
        TargetKind::Server,
        TargetKind::Qworkerd,
        TargetKind::Browser,
        TargetKind::Adapter,
    ] {
        assert_eq!(TargetKind::parse(kind.as_str()).expect("parse"), kind);
        assert_eq!(kind.to_string(), kind.as_str());
    }
    assert_eq!(
        TargetKind::parse("mainframe").expect_err("unknown").code(),
        "VALIDATION_SCHEMA"
    );

    assert_eq!(OutcomeKind::Success.to_string(), "success");
    assert_eq!(OutcomeKind::FailedRetryable.as_str(), "failed_retryable");
    assert_eq!(OutcomeKind::FailedPermanent.as_str(), "failed_permanent");
    assert!(EffectOutcome::failure(true, None, Vec::new()).is_retryable());
    assert!(!EffectOutcome::failure(false, None, Vec::new()).is_retryable());
    assert!(!EffectOutcome::success(Some("ref".into()), Vec::new()).is_retryable());

    for (cell, strategy) in [
        ("none", ReconciliationStrategy::None),
        (
            "idempotent (content digest)",
            ReconciliationStrategy::Idempotent,
        ),
        ("query (stat/digest)", ReconciliationStrategy::Query),
        ("manual (human confirms)", ReconciliationStrategy::Manual),
    ] {
        assert_eq!(
            cell.parse::<ReconciliationStrategy>().expect(cell),
            strategy
        );
        assert_eq!(strategy.to_string(), strategy.as_str());
    }
    assert!(ReconciliationStrategy::Query.is_deterministic());
    assert!(!ReconciliationStrategy::Manual.is_deterministic());
    assert_eq!(
        "guess"
            .parse::<ReconciliationStrategy>()
            .expect_err("unknown")
            .code(),
        "RUNTIME_ILLEGAL_TRANSITION"
    );
    assert_eq!(
        ReconciliationEvidence::Manual {
            evidence_ids: Vec::new()
        }
        .label(),
        "manual"
    );
}
