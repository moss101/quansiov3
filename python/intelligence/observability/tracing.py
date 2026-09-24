"""W3C trace context and structured JSON logs for the intelligence plane (OPS-003).

The runtime sends a `traceparent` in the gRPC metadata of every call. Each RPC here continues it:
the RPC is one span of the same trace, a child of the runtime's call span, and its log lines carry
the trace id — so a run can be followed from the desktop through the runtime into this process and
back. Every line is one JSON object and is redacted as a whole before it is written, whatever field
a secret might have ridden in on; request content is never logged.
"""

from __future__ import annotations

import contextvars
import json
import logging
import secrets
import sys
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from datetime import UTC, datetime

from intelligence.observability.redact import redact_text

TRACEPARENT = "traceparent"
SERVICE = "intelligence"
SPAN_EVENT = "span"

_CURRENT: contextvars.ContextVar[RpcSpan | None] = contextvars.ContextVar("quansio_rpc_span", default=None)


@dataclass(frozen=True, slots=True)
class TraceContext:
    """One span's place in a trace (W3C trace-context)."""

    trace_id: str
    span_id: str
    parent_span_id: str | None = None

    @classmethod
    def root(cls) -> TraceContext:
        return cls(trace_id=_hex(16), span_id=_hex(8))

    @classmethod
    def continue_from(cls, traceparent: str) -> TraceContext | None:
        """Continue `00-<trace>-<parent>-<flags>`; `None` for anything that is not a valid header."""
        parts = traceparent.strip().split("-")
        if len(parts) != 4:
            return None
        version, trace_id, parent, flags = parts
        if version != "00" or len(flags) != 2 or not _valid(trace_id, 32) or not _valid(parent, 16):
            return None
        return cls(trace_id=trace_id, span_id=_hex(8), parent_span_id=parent)

    def child(self) -> TraceContext:
        return TraceContext(trace_id=self.trace_id, span_id=_hex(8), parent_span_id=self.span_id)

    def traceparent(self) -> str:
        return f"00-{self.trace_id}-{self.span_id}-01"


def _valid(value: str, length: int) -> bool:
    return (
        len(value) == length
        and all(character in "0123456789abcdef" for character in value)
        and any(character != "0" for character in value)
    )


def _hex(size: int) -> str:
    value = secrets.token_hex(size)
    return value if any(character != "0" for character in value) else "1" + value[1:]


def traceparent_from(metadata: Sequence[tuple[str, str | bytes]]) -> str:
    """The `traceparent` in gRPC invocation metadata, or ''."""
    for key, value in metadata:
        if key == TRACEPARENT:
            return value.decode("ascii", "replace") if isinstance(value, bytes) else value
    return ""


@dataclass(slots=True)
class RpcSpan:
    """One RPC handled here, continuing the caller's span when it sent one."""

    method: str
    context: TraceContext
    started: float = field(default_factory=time.monotonic)
    fields: dict[str, object] = field(default_factory=dict)
    _token: contextvars.Token[RpcSpan | None] | None = None

    @classmethod
    def begin(cls, method: str, metadata: Sequence[tuple[str, str | bytes]]) -> RpcSpan:
        incoming = traceparent_from(metadata)
        context = (TraceContext.continue_from(incoming) if incoming else None) or TraceContext.root()
        span = cls(method=method, context=context)
        span._token = _CURRENT.set(span)
        return span

    def annotate(self, **fields: object) -> None:
        """Add fields the RPC learned (its tenant, its correlation id) to its span."""
        self.fields.update({key: value for key, value in fields.items() if value})

    def end(self, outcome: str) -> None:
        duration_ms = int((time.monotonic() - self.started) * 1000)
        log_event(
            logging.WARNING if outcome == "error" else logging.INFO,
            SPAN_EVENT,
            context=self.context,
            name="intelligence.handle",
            method=self.method,
            outcome=outcome,
            duration_ms=duration_ms,
            **self.fields,
        )
        if self._token is not None:
            _CURRENT.reset(self._token)
            self._token = None


def current_span() -> RpcSpan | None:
    return _CURRENT.get()


LOGGER_NAME = "intelligence"


def log_event(level: int, event: str, *, context: TraceContext | None = None, **fields: object) -> None:
    """One structured event; the current RPC's trace ids unless `context` is given."""
    span = current_span()
    trace = context or (span.context if span is not None else None)
    logging.getLogger(LOGGER_NAME).log(level, event, extra={"quansio_fields": fields, "quansio_trace": trace})


class JsonFormatter(logging.Formatter):
    """One redacted JSON object per record."""

    def format(self, record: logging.LogRecord) -> str:
        message = record.getMessage()
        line: dict[str, object] = {
            "ts": datetime.fromtimestamp(record.created, UTC)
            .isoformat(timespec="microseconds")
            .replace("+00:00", "Z"),
            "level": record.levelname.lower(),
            "service": SERVICE,
            "event": message,
        }
        # Existing structured records log a JSON object as their message: its fields (its own
        # `event` included) become the line's, rather than an escaped string inside it.
        if message.startswith("{"):
            try:
                structured = json.loads(message)
            except ValueError:
                structured = None
            if isinstance(structured, dict):
                line.update(
                    {str(key): value for key, value in structured.items() if key not in ("ts", "service")}
                )
                line.setdefault("event", "log")
        trace = getattr(record, "quansio_trace", None)
        if trace is None:
            span = current_span()
            trace = span.context if span is not None else None
        if isinstance(trace, TraceContext):
            line["trace_id"] = trace.trace_id
            line["span_id"] = trace.span_id
            if trace.parent_span_id:
                line["parent_span_id"] = trace.parent_span_id
        fields = getattr(record, "quansio_fields", None)
        if isinstance(fields, Mapping):
            for key, value in fields.items():
                line.setdefault(str(key), value)
        if record.name != LOGGER_NAME:
            line.setdefault("logger", record.name)
        if record.exc_info:
            line["error"] = self.formatException(record.exc_info)
        return redact_text(json.dumps(line, sort_keys=True, default=str))


def install_json_logging(level: str = "INFO", stream: object = None) -> None:
    """Route every log record through the redacting JSON formatter."""
    handler = logging.StreamHandler(stream or sys.stderr)  # type: ignore[arg-type]
    handler.setFormatter(JsonFormatter())
    root = logging.getLogger()
    root.handlers = [handler]
    root.setLevel(level.upper())
