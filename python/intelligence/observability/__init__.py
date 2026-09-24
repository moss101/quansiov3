"""Observability for the intelligence plane (OPS-003).

Correlation ids continue the runtime trace. Logs, traces and diagnostic bundles
are redacted so secret canaries never appear.
"""

from __future__ import annotations

from intelligence.observability.correlation import SERVICES, Correlation, JsonLog, Span, continue_trace
from intelligence.observability.redact import (
    CANARY_PLACEHOLDER,
    DEFAULT_SECRET_CANARY,
    DiagnosticBundle,
    MetricsRegistry,
    redact_text,
    secret_canaries,
)
from intelligence.observability.tracing import (
    JsonFormatter,
    RpcSpan,
    TraceContext,
    install_json_logging,
    log_event,
)

__all__ = [
    "CANARY_PLACEHOLDER",
    "DEFAULT_SECRET_CANARY",
    "SERVICES",
    "Correlation",
    "DiagnosticBundle",
    "JsonFormatter",
    "JsonLog",
    "MetricsRegistry",
    "RpcSpan",
    "Span",
    "TraceContext",
    "continue_trace",
    "install_json_logging",
    "log_event",
    "redact_text",
    "secret_canaries",
]
