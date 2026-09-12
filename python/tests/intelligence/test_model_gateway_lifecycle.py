"""Stream lifecycle for the model gateway (INT-002): cancellation, timeout, terminal outcome.

Nothing here is simulated at the transport level: the gateway talks to the loopback stub (or,
for the timeout case, to a socket that accepts and never answers) over real HTTP, and the
assertions are about the provider stream actually being closed and the terminal outcome being
recorded exactly once.
"""

from __future__ import annotations

import time
from collections.abc import Iterator

import pytest
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.conformance import (
    NonRespondingProvider,
    StubProvider,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

KIND = ProviderKind.ANTHROPIC
USAGE_KIND = intelligence_pb2.ModelEvent.KIND_USAGE
STOP_KIND = intelligence_pb2.ModelEvent.KIND_STOP
ERROR_KIND = intelligence_pb2.ModelEvent.KIND_ERROR
DELTA_KIND = intelligence_pb2.ModelEvent.KIND_DELTA


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


def _model_id(gateway: ModelGateway) -> str:
    return catalog_model_for_kind(gateway.catalog, KIND)


def _read_until_delta(fulfillment: object) -> list[int]:
    """Consume events until the first DELTA, asserting the stream started."""
    kinds: list[int] = []
    for event in fulfillment:  # type: ignore[union-attr]
        kinds.append(event.kind)
        if event.kind == DELTA_KIND:
            return kinds
    raise AssertionError("the provider stream produced no delta")


def _await(condition: object, timeout: float = 5.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():  # type: ignore[operator]
            return True
        time.sleep(0.02)
    return bool(condition())  # type: ignore[operator]


def test_cancel_mid_stream_records_a_terminal_cancelled_outcome(
    gateway: ModelGateway, stub: StubProvider
) -> None:
    call = build_call(model_catalog_id=_model_id(gateway), scenario="slow", timeout_ms=10_000)
    fulfillment = gateway.fulfill(call)
    kinds = _read_until_delta(fulfillment)
    assert kinds[0] == intelligence_pb2.ModelEvent.KIND_CALL_STARTED

    fulfillment.cancel()
    remainder = list(fulfillment)
    all_kinds = [*kinds, *(event.kind for event in remainder)]

    assert all_kinds[-1] == intelligence_pb2.ModelEvent.KIND_CALL_COMPLETED
    stops = [event for event in remainder if event.kind == STOP_KIND]
    assert len(stops) == 1
    assert stops[0].stop_reason == intelligence_pb2.MODEL_STOP_REASON_CANCELLED
    usage_events = [event for event in remainder if event.kind == USAGE_KIND]
    assert len(usage_events) == 1
    # The provider reported input usage before the delta; the cancelled call counts it once.
    assert usage_events[0].usage.input_tokens == 11
    assert usage_events[0].usage.cache_read_tokens == 3

    outcome = fulfillment.outcome
    assert outcome is not None and outcome.cancelled
    assert outcome.stop_reason == intelligence_pb2.MODEL_STOP_REASON_CANCELLED
    assert outcome.usage.input_tokens == 11
    assert outcome.usage.output_tokens == 0
    assert outcome.error_code is None

    # Cancellation closed the provider socket: the stub sees the disconnect and no active stream.
    assert _await(lambda: stub.state.active_streams == 0)
    assert stub.state.disconnected_streams >= 1
    # No further events: the generator is finished, not dangling.
    assert list(fulfillment) == []


def test_close_mid_stream_records_a_terminal_outcome(gateway: ModelGateway, stub: StubProvider) -> None:
    call = build_call(model_catalog_id=_model_id(gateway), scenario="slow", timeout_ms=10_000)
    fulfillment = gateway.fulfill(call)
    _read_until_delta(fulfillment)

    fulfillment.close()
    assert fulfillment.closed
    outcome = fulfillment.outcome
    assert outcome is not None and outcome.cancelled
    assert outcome.stop_reason == intelligence_pb2.MODEL_STOP_REASON_CANCELLED
    assert list(fulfillment) == []
    assert _await(lambda: stub.state.active_streams == 0)


def test_cancellation_token_from_the_request_cancels_the_call(
    gateway: ModelGateway, stub: StubProvider
) -> None:
    token = "tok_int002_cancel"
    call = build_call(
        model_catalog_id=_model_id(gateway),
        scenario="slow",
        timeout_ms=10_000,
        cancellation_token=token,
    )
    fulfillment = gateway.fulfill(call)
    _read_until_delta(fulfillment)

    assert gateway.cancel(token) is True
    remainder = list(fulfillment)
    stops = [event for event in remainder if event.kind == STOP_KIND]
    assert stops[0].stop_reason == intelligence_pb2.MODEL_STOP_REASON_CANCELLED
    # The registry entry is released when the call reaches its terminal outcome.
    assert gateway.cancel(token) is False
    assert _await(lambda: stub.state.active_streams == 0)


def test_provider_that_never_responds_is_terminated_with_a_typed_timeout() -> None:
    with NonRespondingProvider() as provider:
        gateway = ModelGateway(
            catalog=stub_catalog(provider.base_url),
            environ=credential_environ(),
            tool_schemas=conformance_tool_schemas(),
        )
        call = build_call(model_catalog_id=_model_id(gateway), scenario="text", timeout_ms=300)
        started_at = time.monotonic()
        events = list(gateway.fulfill(call))
        elapsed = time.monotonic() - started_at

    assert elapsed < 5.0, "a silent provider must be bounded by the call budget"
    errors = [event for event in events if event.kind == ERROR_KIND]
    assert len(errors) == 1
    assert errors[0].error.code == "TOOL_TIMEOUT"
    assert errors[0].error.retryable is True
    stops = [event for event in events if event.kind == STOP_KIND]
    assert stops[0].stop_reason == intelligence_pb2.MODEL_STOP_REASON_ERROR
    assert events[-1].kind == intelligence_pb2.ModelEvent.KIND_CALL_COMPLETED


def test_call_deadline_bounds_a_stalled_stream(gateway: ModelGateway, stub: StubProvider) -> None:
    call = build_call(model_catalog_id=_model_id(gateway), scenario="slow", timeout_ms=10_000)
    deadline_ms = int(time.time() * 1000) + 400
    started_at = time.monotonic()
    events = list(gateway.fulfill(call, deadline_ms=deadline_ms))
    elapsed = time.monotonic() - started_at

    assert elapsed < 5.0
    errors = [event for event in events if event.kind == ERROR_KIND]
    assert errors and errors[0].error.code == "TOOL_TIMEOUT"
    assert _await(lambda: stub.state.active_streams == 0)


def test_expired_deadline_fails_before_any_provider_call(gateway: ModelGateway, stub: StubProvider) -> None:
    call = build_call(model_catalog_id=_model_id(gateway), scenario="text")
    before = len(stub.state.calls)
    with pytest.raises(GatewayError) as excinfo:
        gateway.fulfill(call, deadline_ms=int(time.time() * 1000) - 1_000)
    assert excinfo.value.code is GatewayErrorCode.TOOL_TIMEOUT
    assert excinfo.value.retryable is False
    assert len(stub.state.calls) == before


def test_usage_is_recorded_once_even_when_the_provider_repeats_it(
    gateway: ModelGateway, stub: StubProvider
) -> None:
    """The stub sends the same usage payload twice; the call must not double-count it."""
    call = build_call(model_catalog_id=_model_id(gateway), scenario="usage_repeat")
    fulfillment = gateway.fulfill(call)
    events = list(fulfillment)
    usage_events = [event for event in events if event.kind == USAGE_KIND]
    assert len(usage_events) == 1
    usage = usage_events[0].usage
    assert (usage.input_tokens, usage.output_tokens, usage.cache_read_tokens) == (11, 7, 3)
    outcome = fulfillment.outcome
    assert outcome is not None
    assert outcome.usage.input_tokens == usage.input_tokens
    assert outcome.usage.output_tokens == usage.output_tokens
    assert outcome.usage.cache_read_tokens == usage.cache_read_tokens


def test_terminal_outcome_is_recorded_exactly_once(gateway: ModelGateway) -> None:
    fulfillment = gateway.fulfill(build_call(model_catalog_id=_model_id(gateway), scenario="text"))
    first = fulfillment.outcome is None
    list(fulfillment)
    assert first, "the outcome is unset while the stream is open"
    outcome = fulfillment.outcome
    assert outcome is not None
    fulfillment.close()
    assert fulfillment.outcome is outcome, "the terminal outcome is never replaced"
