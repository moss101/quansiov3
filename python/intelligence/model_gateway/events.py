"""Normalized `ModelEvent` construction and gateway accounting types (DOMAIN.md §11.1).

Adapters decode provider payloads into normalized events through `EventFactory`, so every
adapter emits exactly the same event kinds, in the same order, with the same identity fields.
`UsageTotals` treats provider counters as cumulative per call and combines fields with `max`:
a provider that repeats or splits a usage payload can never inflate the recorded usage.

Cost estimation is a coarse band derived from the catalog `cost_class`; the authoritative
usage/billing figures remain the Rust usage projection (DOSSIER.md §5), and the exact
price table is INT-003/OPS-004 work. Bands are minor units per million tokens.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, cast

from quansio.v1.intelligence import intelligence_pb2

SCHEMA_VERSION = "v1"

# Provisional spend bands per million tokens by catalog cost_class (INT-003/OPS-004 own pricing).
COST_CLASS_MINOR_UNITS_PER_MILLION: dict[str, int] = {
    "low": 100,
    "standard": 1_000,
    "premium": 5_000,
}
_DEFAULT_COST_BAND = 1_000


@dataclass(frozen=True, slots=True)
class UsageTotals:
    """Token counters for one call, including prompt-cache reads and writes."""

    input_tokens: int = 0
    output_tokens: int = 0
    cache_read_tokens: int = 0
    cache_write_tokens: int = 0

    def combine(self, other: UsageTotals) -> UsageTotals:
        """Field-wise maximum: usage payloads are cumulative, so this cannot double-count."""
        return UsageTotals(
            input_tokens=max(self.input_tokens, other.input_tokens),
            output_tokens=max(self.output_tokens, other.output_tokens),
            cache_read_tokens=max(self.cache_read_tokens, other.cache_read_tokens),
            cache_write_tokens=max(self.cache_write_tokens, other.cache_write_tokens),
        )

    @property
    def total_tokens(self) -> int:
        return self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens

    def to_proto(self) -> intelligence_pb2.Usage:
        return intelligence_pb2.Usage(
            schema_version=SCHEMA_VERSION,
            input_tokens=self.input_tokens,
            output_tokens=self.output_tokens,
            cache_read_tokens=self.cache_read_tokens,
            cache_write_tokens=self.cache_write_tokens,
        )


def estimate_cost_minor_units(usage: UsageTotals, cost_class: str) -> int:
    """Coarse spend estimate in minor units for one call (see module docstring)."""
    band = COST_CLASS_MINOR_UNITS_PER_MILLION.get(cost_class, _DEFAULT_COST_BAND)
    return (usage.total_tokens * band) // 1_000_000


class EventFactory:
    """Builds normalized `ModelEvent` messages bound to one call and route.

    The factory also accumulates the usage the provider has reported so far, so a cancelled
    stream can record what it actually saw instead of guessing. Accumulation is `max`-based
    (see `UsageTotals.combine`), so repeated or split usage payloads cannot double-count.
    """

    __slots__ = ("_call_id", "_route_id", "_usage")

    def __init__(self, call_id: str, route_id: str) -> None:
        self._call_id = call_id
        self._route_id = route_id
        self._usage = UsageTotals()

    @property
    def call_id(self) -> str:
        return self._call_id

    @property
    def route_id(self) -> str:
        return self._route_id

    @property
    def partial_usage(self) -> UsageTotals:
        return self._usage

    def observe_usage(self, totals: UsageTotals) -> UsageTotals:
        """Record a provider usage payload; returns the accumulated totals."""
        self._usage = self._usage.combine(totals)
        return self._usage

    def _base(self, kind: intelligence_pb2.ModelEvent.Kind) -> intelligence_pb2.ModelEvent:
        return intelligence_pb2.ModelEvent(
            schema_version=SCHEMA_VERSION, call_id=self._call_id, route_id=self._route_id, kind=kind
        )

    def started(self) -> intelligence_pb2.ModelEvent:
        return self._base(intelligence_pb2.ModelEvent.KIND_CALL_STARTED)

    def delta(self, text: str) -> intelligence_pb2.ModelEvent:
        event = self._base(intelligence_pb2.ModelEvent.KIND_DELTA)
        event.text_delta = text
        return event

    def tool_call(self, tool_call_id: str, name: str, args_json: str) -> intelligence_pb2.ModelEvent:
        event = self._base(intelligence_pb2.ModelEvent.KIND_TOOL_CALL)
        event.tool_call_id = tool_call_id
        event.tool_name = name
        event.tool_args_json = args_json
        return event

    def thinking_summary(self, text: str) -> intelligence_pb2.ModelEvent:
        event = self._base(intelligence_pb2.ModelEvent.KIND_THINKING_SUMMARY)
        event.text_delta = text
        return event

    def usage(self, totals: UsageTotals) -> intelligence_pb2.ModelEvent:
        event = self._base(intelligence_pb2.ModelEvent.KIND_USAGE)
        event.usage.CopyFrom(totals.to_proto())
        return event

    def stop(self, reason: int) -> intelligence_pb2.ModelEvent:
        event = self._base(intelligence_pb2.ModelEvent.KIND_STOP)
        event.stop_reason = cast(Any, reason)
        return event

    def error(self, code: str, *, retryable: bool) -> intelligence_pb2.ModelEvent:
        event = self._base(intelligence_pb2.ModelEvent.KIND_ERROR)
        event.error.schema_version = SCHEMA_VERSION
        event.error.code = code
        event.error.retryable = retryable
        return event

    def completed(self, *, latency_ms: int, cost_estimate_minor_units: int) -> intelligence_pb2.ModelEvent:
        event = self._base(intelligence_pb2.ModelEvent.KIND_CALL_COMPLETED)
        event.latency_ms = latency_ms
        event.cost_estimate_minor_units = cost_estimate_minor_units
        return event


@dataclass(frozen=True, slots=True)
class TerminalOutcome:
    """The single terminal outcome recorded for a call (exactly once, never double-counted)."""

    call_id: str
    route_id: str
    stop_reason: int
    usage: UsageTotals
    latency_ms: int
    cost_estimate_minor_units: int
    cancelled: bool = False
    error_code: str | None = None
    retryable: bool = False

    @property
    def terminal(self) -> bool:
        return True
