//! OPS-003: correlated traces, secret canaries, bounded diagnostic bundles.

use quansio_server::observability::{
    continue_trace, redact_text, Correlation, DiagnosticBundle, JsonLog, MetricsRegistry, Span,
    CANARY_PLACEHOLDER, DEFAULT_BUNDLE_LIMIT, DEFAULT_SECRET_CANARY,
};

#[test]
fn a_run_is_traced_end_to_end_across_service_boundaries() {
    let root = Correlation::start("corr_run_1", "tn_alpha", Some("run_alpha".into()));
    let runtime_span = Span::open(&root, "span_runtime", "run.start");
    let intel = continue_trace(&root, "intelligence").expect("intelligence is a plane");
    let intel_span = runtime_span.child(&intel, "span_intel", "model.call");
    let desktop = continue_trace(&root, "desktop").expect("desktop is a plane");
    let desktop_span = intel_span.child(&desktop, "span_desktop", "timeline.render");

    assert_eq!(runtime_span.correlation_id, intel_span.correlation_id);
    assert_eq!(intel_span.correlation_id, desktop_span.correlation_id);
    assert_eq!(intel_span.parent_span_id.as_deref(), Some("span_runtime"));
    assert_eq!(desktop_span.parent_span_id.as_deref(), Some("span_intel"));
    assert_eq!(intel.service, "intelligence");
    assert_eq!(desktop.service, "desktop");
    assert!(continue_trace(&root, "billing").is_err());
}

#[test]
fn secret_canary_values_do_not_appear_in_logs_traces_or_bundles() {
    let canary = DEFAULT_SECRET_CANARY;
    let root = Correlation::start("corr_secret", "tn_alpha", None);
    let log = JsonLog::emit(&root, "info", &format!("auth {canary} Bearer tokensecret"));
    assert!(
        !log.msg.contains(canary),
        "canary leaked into log: {}",
        log.msg
    );
    assert!(!log.to_json().contains(canary));
    assert!(log.msg.contains(CANARY_PLACEHOLDER));
    assert!(log.to_json().contains("correlation_id"));

    let span = Span::open(&root, "span_1", format!("call with {canary}"));
    assert!(!span.name.contains(canary));

    let mut metrics = MetricsRegistry::default();
    metrics.incr("quansio_runs_total", canary);
    let expo = metrics.prometheus();
    assert!(!expo.contains(canary));
    assert!(expo.contains("# TYPE quansio_runs_total counter"));

    let bundle = DiagnosticBundle::export(
        root.correlation_id.clone(),
        vec![span],
        vec![log],
        &metrics,
        DEFAULT_BUNDLE_LIMIT,
    );
    let json = bundle.to_json();
    assert!(!json.contains(canary));
    assert!(!redact_text(&format!("keep {canary} out")).contains(canary));
}

#[test]
fn diagnostic_bundle_is_bounded_and_redacted() {
    let root = Correlation::start("corr_bundle", "tn_alpha", Some("run_b".into()));
    let logs: Vec<_> = (0..50)
        .map(|i| JsonLog::emit(&root, "info", &format!("line {i} {DEFAULT_SECRET_CANARY}")))
        .collect();
    let spans: Vec<_> = (0..20)
        .map(|i| Span::open(&root, format!("span_{i}"), format!("op {i}")))
        .collect();
    let metrics = MetricsRegistry::default();
    let tiny = DiagnosticBundle::export(root.correlation_id.clone(), spans, logs, &metrics, 800);
    assert!(tiny.truncated);
    assert!(
        tiny.to_json().len() <= 800 + 8,
        "bundle should sit near the cap"
    );
    assert!(!tiny.to_json().contains(DEFAULT_SECRET_CANARY));
}
