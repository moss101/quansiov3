"""OPS-003 in the intelligence plane: each RPC continues the caller's trace, in redacted JSON logs.

The cross-process proof (runtime → this process, with parent links) is
`crates/server/tests/observability.rs`; these pin the plane's own half directly and over a real
gRPC channel.
"""

from __future__ import annotations

import io
import json
import logging
import time
from collections.abc import Iterator

import grpc
import pytest
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.observability.redact import DEFAULT_SECRET_CANARY
from intelligence.observability.tracing import (
    JsonFormatter,
    TraceContext,
    install_json_logging,
    log_event,
)
from intelligence.server import IntelligenceGatewayServicer, IntelligenceServer, ServerConfig, Transport

TRACE = "4bf92f3577b34da6a3ce929d0e0e4736"
PARENT = "00f067aa0ba902b7"


def test_a_traceparent_is_continued_and_a_malformed_one_is_not() -> None:
    continued = TraceContext.continue_from(f"00-{TRACE}-{PARENT}-01")
    assert continued is not None
    assert continued.trace_id == TRACE and continued.parent_span_id == PARENT
    assert len(continued.span_id) == 16 and continued.span_id != PARENT
    child = continued.child()
    assert child.trace_id == TRACE and child.parent_span_id == continued.span_id
    for bad in (
        "",
        "garbage",
        f"01-{TRACE}-{PARENT}-01",
        f"00-{'0' * 32}-{PARENT}-01",
        f"00-{TRACE.upper()}-{PARENT}-01",
    ):
        assert TraceContext.continue_from(bad) is None, bad


@pytest.fixture
def captured() -> Iterator[io.StringIO]:
    stream = io.StringIO()
    root = logging.getLogger()
    saved = (root.handlers[:], root.level)
    install_json_logging("INFO", stream)
    yield stream
    root.handlers, root.level = saved[0], saved[1]


def lines(stream: io.StringIO) -> list[dict[str, object]]:
    return [json.loads(line) for line in stream.getvalue().splitlines() if line.strip()]


def test_every_line_is_redacted_json_and_structured_messages_merge(captured: io.StringIO) -> None:
    log_event(logging.INFO, "probe", note=f"key {DEFAULT_SECRET_CANARY}", context=TraceContext(TRACE, PARENT))
    logging.getLogger("intelligence.server").info(json.dumps({"event": "started", "address": "x"}))
    first, second = lines(captured)
    assert first["event"] == "probe" and first["trace_id"] == TRACE and first["service"] == "intelligence"
    assert DEFAULT_SECRET_CANARY not in captured.getvalue()
    assert second["event"] == "started" and second["address"] == "x"
    assert (
        JsonFormatter()
        .format(logging.LogRecord("x", logging.INFO, "", 0, "plain", None, None))
        .startswith("{")
    )


def _scope() -> service_pb2.ScopeContext:
    return service_pb2.ScopeContext(
        schema_version="v1",
        tenant_id="tn_01J8Z3K6F1N8VQ2X5W9Y0PS003",
        workspace_id="ws_01J8Z3K6F1N8VQ2X5W9Y0PS003",
        correlation_id="corr_ops003",
        deadline_ms=int(time.time() * 1000) + 30_000,
        capability_projection_id="cap_01J8Z3K6F1N8VQ2X5W9Y0PS003",
    )


def test_each_rpc_is_a_span_of_the_callers_trace(captured: io.StringIO) -> None:
    server = IntelligenceServer(
        ServerConfig(transport=Transport.TCP, host="127.0.0.1", port=0),
        servicer=IntelligenceGatewayServicer(),
    )
    address = server.start()
    channel = grpc.insecure_channel(address)
    try:
        grpc.channel_ready_future(channel).result(timeout=10.0)
        client = service_pb2_grpc.IntelligenceGatewayStub(channel)
        request = service_pb2.TrustClassifyRequest(
            schema_version="v1",
            scope=_scope(),
            source_kind="web_page",
            content=f"hello {DEFAULT_SECRET_CANARY}",
        )
        client.ClassifyTrust(request, metadata=(("traceparent", f"00-{TRACE}-{PARENT}-01"),))
        client.ClassifyTrust(request)  # no traceparent: a trace of its own
        unscoped = service_pb2.TrustClassifyRequest(schema_version="v1", source_kind="web_page", content="x")
        with pytest.raises(grpc.RpcError):
            client.ClassifyTrust(unscoped, metadata=(("traceparent", f"00-{TRACE}-{PARENT}-01"),))
    finally:
        channel.close()
        server.stop(2.0)
    spans = [line for line in lines(captured) if line.get("event") == "span"]
    traced = [span for span in spans if span.get("trace_id") == TRACE]
    assert len(traced) == 2, spans
    ok, refused = traced
    assert ok["parent_span_id"] == PARENT and ok["method"] == "ClassifyTrust" and ok["outcome"] == "ok"
    assert ok["tenant_id"] == "tn_01J8Z3K6F1N8VQ2X5W9Y0PS003" and ok["correlation_id"] == "corr_ops003"
    assert refused["outcome"] == "error", "an aborted call ends its span as an error"
    others = [span for span in spans if span.get("trace_id") != TRACE]
    assert len(others) == 1 and "parent_span_id" not in others[0]
    assert DEFAULT_SECRET_CANARY not in captured.getvalue()
