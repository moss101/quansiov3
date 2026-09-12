"""`FulfillModel` through the real gRPC boundary (INT-001 servicer + INT-002 gateway).

Every call here goes over a real gRPC channel to a real `IntelligenceServer` whose servicer
holds a `ModelGateway` pointed at the loopback conformance stub. The INT-001 scope/deadline
gate, the typed error envelope and the streaming contract are exercised together.
"""

from __future__ import annotations

import contextlib
import time
from collections.abc import Iterator
from typing import Any

import grpc
import pytest
from quansio.v1.intelligence import intelligence_pb2, service_pb2, service_pb2_grpc

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.conformance import (
    StubProvider,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    stub_catalog,
)
from intelligence.server import (
    SCOPE_METADATA_KEY,
    ErrorCode,
    IntelligenceGatewayServicer,
    IntelligenceServer,
    ServerConfig,
    Transport,
    decode_error,
)

TENANT_ID = "tn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"
WORKSPACE_ID = "ws_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"
CORRELATION_ID = "corr_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"
CAPABILITY_PROJECTION_ID = "cp_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


@pytest.fixture(scope="module")
def gateway(stub: StubProvider) -> ModelGateway:
    return ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ=credential_environ(),
        tool_schemas=conformance_tool_schemas(),
    )


def scope_metadata(deadline_ms: int) -> tuple[tuple[str, bytes], ...]:
    scope = service_pb2.ScopeContext(
        schema_version="v1",
        tenant_id=TENANT_ID,
        workspace_id=WORKSPACE_ID,
        correlation_id=CORRELATION_ID,
        deadline_ms=deadline_ms,
        capability_projection_id=CAPABILITY_PROJECTION_ID,
    )
    return ((SCOPE_METADATA_KEY, scope.SerializeToString()),)


def call_for(gateway: ModelGateway, scenario: str, **overrides: Any) -> intelligence_pb2.ModelCallRequest:
    request = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC),
        scenario=scenario,
        timeout_ms=10_000,
        tools=scenario == "tool_call",
    )
    for field, value in overrides.items():
        setattr(request, field, value)
    return request


@contextlib.contextmanager
def running_gateway(gateway: ModelGateway | None) -> Iterator[Any]:
    servicer = IntelligenceGatewayServicer(gateway=gateway)
    server = IntelligenceServer(
        ServerConfig(transport=Transport.TCP, host="127.0.0.1", port=0), servicer=servicer
    )
    address = server.start()
    channel = grpc.insecure_channel(address)
    try:
        grpc.channel_ready_future(channel).result(timeout=10.0)
        yield server, service_pb2_grpc.IntelligenceGatewayStub(channel)
    finally:
        channel.close()
        server.stop(2.0)


def test_fulfill_model_streams_normalized_events(gateway: ModelGateway) -> None:
    with running_gateway(gateway) as (_server, stub_client):
        events = list(
            stub_client.FulfillModel(
                call_for(gateway, "text"),
                metadata=scope_metadata(int(time.time() * 1000) + 30_000),
            )
        )
    kinds = [event.kind for event in events]
    assert kinds[0] == intelligence_pb2.ModelEvent.KIND_CALL_STARTED
    assert kinds[-1] == intelligence_pb2.ModelEvent.KIND_CALL_COMPLETED
    assert kinds.count(intelligence_pb2.ModelEvent.KIND_USAGE) == 1
    assert kinds.count(intelligence_pb2.ModelEvent.KIND_STOP) == 1
    assert [event.text_delta for event in events if event.kind == intelligence_pb2.ModelEvent.KIND_DELTA] == [
        "Hello ",
        "world",
    ]
    assert events[0].call_id == call_for(gateway, "text").call_id
    assert events[0].route_id.startswith("mr_")
    assert all(event.call_id == events[0].call_id for event in events)


def test_fulfill_model_tool_call_survives_the_boundary(gateway: ModelGateway) -> None:
    with running_gateway(gateway) as (_server, stub_client):
        events = list(
            stub_client.FulfillModel(
                call_for(gateway, "tool_call"),
                metadata=scope_metadata(int(time.time() * 1000) + 30_000),
            )
        )
    tool_calls = [event for event in events if event.kind == intelligence_pb2.ModelEvent.KIND_TOOL_CALL]
    assert len(tool_calls) == 1
    assert tool_calls[0].tool_name == "fs.read"


def test_fulfill_model_requires_the_scope_trailer(gateway: ModelGateway) -> None:
    with running_gateway(gateway) as (_server, stub_client), pytest.raises(grpc.RpcError) as excinfo:
        list(stub_client.FulfillModel(call_for(gateway, "text")))
    assert excinfo.value.code() == grpc.StatusCode.INVALID_ARGUMENT
    assert decode_error(excinfo.value).code is ErrorCode.VALIDATION_SCHEMA


def test_fulfill_model_without_a_gateway_fails_closed() -> None:
    with running_gateway(None) as (_server, stub_client), pytest.raises(grpc.RpcError) as excinfo:
        list(
            stub_client.FulfillModel(
                intelligence_pb2.ModelCallRequest(schema_version="v1"),
                metadata=scope_metadata(int(time.time() * 1000) + 30_000),
            )
        )
    assert excinfo.value.code() == grpc.StatusCode.FAILED_PRECONDITION
    assert decode_error(excinfo.value).code is ErrorCode.ROUTE_UNAVAILABLE


def test_fulfill_model_reports_an_unresolvable_route(gateway: ModelGateway) -> None:
    with running_gateway(gateway) as (_server, stub_client):
        request = call_for(gateway, "text", route_hint="no-such-catalog-model")
        with pytest.raises(grpc.RpcError) as excinfo:
            list(stub_client.FulfillModel(request, metadata=scope_metadata(int(time.time() * 1000) + 30_000)))
    assert excinfo.value.code() == grpc.StatusCode.FAILED_PRECONDITION
    envelope = decode_error(excinfo.value)
    assert envelope.code is ErrorCode.ROUTE_UNAVAILABLE
    assert {detail.key: detail.value for detail in envelope.details}["owner_task"] == "INT-002"


def test_expired_scope_deadline_is_rejected_before_the_provider(
    gateway: ModelGateway, stub: StubProvider
) -> None:
    before = len(stub.state.calls)
    with running_gateway(gateway) as (_server, stub_client), pytest.raises(grpc.RpcError) as excinfo:
        list(
            stub_client.FulfillModel(
                call_for(gateway, "text"),
                metadata=scope_metadata(int(time.time() * 1000) - 1_000),
            )
        )
    assert excinfo.value.code() == grpc.StatusCode.DEADLINE_EXCEEDED
    assert decode_error(excinfo.value).code is ErrorCode.TOOL_TIMEOUT
    assert len(stub.state.calls) == before


def test_scope_deadline_bounds_a_stalled_provider_stream(gateway: ModelGateway) -> None:
    with running_gateway(gateway) as (_server, stub_client):
        events = list(
            stub_client.FulfillModel(
                call_for(gateway, "slow"),
                metadata=scope_metadata(int(time.time() * 1000) + 400),
            )
        )
    errors = [event for event in events if event.kind == intelligence_pb2.ModelEvent.KIND_ERROR]
    assert errors and errors[0].error.code == "TOOL_TIMEOUT"
    stops = [event for event in events if event.kind == intelligence_pb2.ModelEvent.KIND_STOP]
    assert stops[0].stop_reason == intelligence_pb2.MODEL_STOP_REASON_ERROR


def test_cancelling_by_token_closes_the_provider_stream(gateway: ModelGateway, stub: StubProvider) -> None:
    """`ModelCallRequest.cancellation_token` is the canonical cancellation path (DOMAIN §11.1)."""
    token = "tok_int002_boundary_cancel"
    with running_gateway(gateway) as (server, stub_client):
        pending = stub_client.FulfillModel(
            call_for(gateway, "slow", cancellation_token=token),
            metadata=scope_metadata(int(time.time() * 1000) + 30_000),
        )
        seen = 0
        for _event in pending:
            seen += 1
            if seen >= 2:
                break
        assert gateway.cancel(token) is True, "the in-flight call must be registered by token"
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline and (
            server.servicer.in_flight > 0 or stub.state.active_streams > 0
        ):
            time.sleep(0.02)
        assert seen >= 2
        assert server.servicer.in_flight == 0, "the cancelled call must leave no in-flight work"
        assert stub.state.active_streams == 0, "the provider stream must be closed"
        assert gateway.cancel(token) is False, "the terminal call releases its token"


def test_servicer_surface_is_unchanged_by_the_gateway(gateway: ModelGateway) -> None:
    servicer = IntelligenceGatewayServicer(gateway=gateway)
    callables = {
        name for name in dir(servicer) if not name.startswith("_") and callable(getattr(servicer, name))
    }
    generated = {
        method.name for method in service_pb2.DESCRIPTOR.services_by_name["IntelligenceGateway"].methods
    }
    assert callables - generated == {"begin_drain", "wait_for_idle"}
