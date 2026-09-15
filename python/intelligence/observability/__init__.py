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

__all__ = [
    "CANARY_PLACEHOLDER",
    "DEFAULT_SECRET_CANARY",
    "SERVICES",
    "Correlation",
    "DiagnosticBundle",
    "JsonLog",
    "MetricsRegistry",
    "Span",
    "continue_trace",
    "redact_text",
    "secret_canaries",
]
