"""OPS-003 intelligence-plane observability: traces, canaries, diagnostic bundles."""

from __future__ import annotations

from intelligence.observability import (
    CANARY_PLACEHOLDER,
    DEFAULT_SECRET_CANARY,
    Correlation,
    DiagnosticBundle,
    JsonLog,
    MetricsRegistry,
    Span,
    continue_trace,
    redact_text,
)


def test_trace_propagation_across_planes() -> None:
    root = Correlation.start("corr_run_1", "tn_alpha", "run_alpha")
    runtime = Span.open(root, "span_runtime", "run.start")
    intel = continue_trace(root, "intelligence")
    intel_span = runtime.child(intel, "span_intel", "model.call")
    desktop = continue_trace(root, "desktop")
    desktop_span = intel_span.child(desktop, "span_desktop", "timeline.render")
    assert runtime.correlation_id == intel_span.correlation_id == desktop_span.correlation_id
    assert intel_span.parent_span_id == "span_runtime"
    assert desktop_span.parent_span_id == "span_intel"


def test_secret_canary_scan() -> None:
    text = redact_text(f"Bearer abc sk-live-1 user@x.test {DEFAULT_SECRET_CANARY}")
    assert DEFAULT_SECRET_CANARY not in text
    assert "Bearer abc" not in text
    assert "sk-live" not in text
    assert "user@x.test" not in text
    assert CANARY_PLACEHOLDER in text
    root = Correlation.start("corr_s", "tn_alpha")
    log = JsonLog.emit(root, "info", f"auth {DEFAULT_SECRET_CANARY}")
    assert DEFAULT_SECRET_CANARY not in log.msg
    assert DEFAULT_SECRET_CANARY not in str(log.as_json())


def test_diagnostic_bundle() -> None:
    root = Correlation.start("corr_b", "tn_alpha", "run_b")
    logs = [JsonLog.emit(root, "info", f"line {i} {DEFAULT_SECRET_CANARY}") for i in range(40)]
    spans = [Span.open(root, f"span_{i}", f"op {i}") for i in range(10)]
    metrics = MetricsRegistry()
    metrics.incr("quansio_runs_total", DEFAULT_SECRET_CANARY)
    bundle = DiagnosticBundle.export(root.correlation_id, spans, logs, metrics, max_bytes=600)
    payload = bundle.to_json()
    assert DEFAULT_SECRET_CANARY not in payload
    assert bundle.truncated
    assert "quansio_runs_total" in bundle.metrics
    assert CANARY_PLACEHOLDER in bundle.metrics
