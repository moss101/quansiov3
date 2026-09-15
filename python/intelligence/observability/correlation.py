"""Correlation context continued across runtime, intelligence and desktop."""

from __future__ import annotations

from dataclasses import dataclass

from intelligence.observability.redact import redact_text

SERVICES: tuple[str, ...] = ("runtime", "intelligence", "desktop")


@dataclass(frozen=True, slots=True)
class Correlation:
    correlation_id: str
    tenant_id: str
    service: str
    run_id: str | None = None

    @classmethod
    def start(cls, correlation_id: str, tenant_id: str, run_id: str | None = None) -> Correlation:
        return cls(correlation_id=correlation_id, tenant_id=tenant_id, service="runtime", run_id=run_id)


def continue_trace(parent: Correlation, service: str) -> Correlation:
    if service not in SERVICES:
        raise ValueError(f"{service} is not a Quansio plane")
    return Correlation(
        correlation_id=parent.correlation_id,
        tenant_id=parent.tenant_id,
        service=service,
        run_id=parent.run_id,
    )


@dataclass(frozen=True, slots=True)
class Span:
    span_id: str
    correlation_id: str
    service: str
    name: str
    parent_span_id: str | None = None

    @classmethod
    def open(cls, correlation: Correlation, span_id: str, name: str) -> Span:
        return cls(
            span_id=span_id,
            correlation_id=correlation.correlation_id,
            service=correlation.service,
            name=redact_text(name),
        )

    def child(self, correlation: Correlation, span_id: str, name: str) -> Span:
        return Span(
            span_id=span_id,
            correlation_id=correlation.correlation_id,
            service=correlation.service,
            name=redact_text(name),
            parent_span_id=self.span_id,
        )

    def as_json(self) -> dict[str, object]:
        return {
            "span_id": self.span_id,
            "parent_span_id": self.parent_span_id,
            "correlation_id": self.correlation_id,
            "service": self.service,
            "name": self.name,
        }


@dataclass(frozen=True, slots=True)
class JsonLog:
    level: str
    correlation_id: str
    service: str
    msg: str

    @classmethod
    def emit(cls, correlation: Correlation, level: str, msg: str) -> JsonLog:
        return cls(
            level=level,
            correlation_id=correlation.correlation_id,
            service=correlation.service,
            msg=redact_text(msg),
        )

    def as_json(self) -> dict[str, str]:
        return {
            "level": self.level,
            "correlation_id": self.correlation_id,
            "service": self.service,
            "msg": self.msg,
        }
