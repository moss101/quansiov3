"""Secret/PII redaction and bounded diagnostic bundles (OPS-003)."""

from __future__ import annotations

import json
import os
import re
from collections.abc import Sequence
from dataclasses import dataclass, field

DEFAULT_SECRET_CANARY = "qncy_test_canary_not_for_prod"
CANARY_PLACEHOLDER = "[REDACTED:canary]"
DEFAULT_BUNDLE_LIMIT = 64 * 1024

_TOKEN_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"Bearer\s+\S+"), "[REDACTED:bearer]"),
    (re.compile(r"\bsk-[A-Za-z0-9_-]+"), "[REDACTED:secret]"),
    (re.compile(r"\bghp_[A-Za-z0-9]+"), "[REDACTED:secret]"),
    (re.compile(r"\bxoxb-[A-Za-z0-9-]+"), "[REDACTED:secret]"),
    (re.compile(r"\bAKIA[0-9A-Z]{16}"), "[REDACTED:secret]"),
    (re.compile(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"), "[REDACTED:email]"),
)


def secret_canaries() -> tuple[str, ...]:
    extra = os.environ.get("QUANSIO_TEST_SECRET_CANARY", "").strip()
    if extra and extra != DEFAULT_SECRET_CANARY:
        return (DEFAULT_SECRET_CANARY, extra)
    return (DEFAULT_SECRET_CANARY,)


def redact_text(value: str) -> str:
    out = value
    for canary in secret_canaries():
        if canary:
            out = out.replace(canary, CANARY_PLACEHOLDER)
    for pattern, placeholder in _TOKEN_PATTERNS:
        out = pattern.sub(placeholder, out)
    return out


@dataclass
class MetricsRegistry:
    rows: list[tuple[str, str, int]] = field(default_factory=list)

    def incr(self, name: str, label: str) -> None:
        name = redact_text(name)
        label = redact_text(label)
        for index, (existing_name, existing_label, value) in enumerate(self.rows):
            if existing_name == name and existing_label == label:
                self.rows[index] = (name, label, value + 1)
                return
        self.rows.append((name, label, 1))

    def prometheus(self) -> str:
        lines = [
            f'# TYPE {name} counter\n{name}{{label="{label}"}} {value}' for name, label, value in self.rows
        ]
        return "\n".join(lines) + ("\n" if lines else "")


@dataclass(frozen=True)
class DiagnosticBundle:
    correlation_id: str
    spans: tuple[object, ...]
    logs: tuple[object, ...]
    metrics: str
    truncated: bool

    def to_json(self) -> str:
        def as_map(item: object) -> object:
            as_json = getattr(item, "as_json", None)
            return as_json() if callable(as_json) else item

        return json.dumps(
            {
                "correlation_id": self.correlation_id,
                "spans": [as_map(item) for item in self.spans],
                "logs": [as_map(item) for item in self.logs],
                "metrics": self.metrics,
                "truncated": self.truncated,
            },
            separators=(",", ":"),
        )

    @classmethod
    def export(
        cls,
        correlation_id: str,
        spans: Sequence[object],
        logs: Sequence[object],
        metrics: MetricsRegistry,
        max_bytes: int = DEFAULT_BUNDLE_LIMIT,
    ) -> DiagnosticBundle:
        kept_spans = list(spans)
        kept_logs = list(logs)
        truncated = False
        bundle = cls(
            correlation_id=correlation_id,
            spans=tuple(kept_spans),
            logs=tuple(kept_logs),
            metrics=redact_text(metrics.prometheus()),
            truncated=False,
        )
        while len(bundle.to_json()) > max_bytes and (kept_logs or kept_spans):
            truncated = True
            if len(kept_logs) >= len(kept_spans) and kept_logs:
                kept_logs.pop()
            elif kept_spans:
                kept_spans.pop()
            else:
                break
            bundle = cls(
                correlation_id=correlation_id,
                spans=tuple(kept_spans),
                logs=tuple(kept_logs),
                metrics=bundle.metrics,
                truncated=truncated,
            )
        return bundle
