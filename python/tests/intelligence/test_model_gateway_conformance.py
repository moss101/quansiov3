"""Adapter conformance for the model gateway (INT-002), offline against the stub provider.

The deterministic `StubProvider` speaks both wire formats over loopback HTTP/SSE, so the three
adapters are exercised through real transport, real framing and the real normalization path.
This is conformance evidence, not real-boundary evidence: the live provider suite is
`test_model_gateway_live.py` and is gated on `QUANSIO_TEST_*`.
"""

from __future__ import annotations

import json
from collections.abc import Iterator

import pytest
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import (
    STUB_PROVIDER_KINDS,
    STUB_SCENARIOS,
    StubProvider,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    run_scenario,
    stub_catalog,
)

KIND_IDS = [kind.value for kind in STUB_PROVIDER_KINDS]
KIND_IDS_BY_VALUE = {kind.value: kind for kind in STUB_PROVIDER_KINDS}


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


@pytest.mark.parametrize("kind_value", KIND_IDS)
@pytest.mark.parametrize("scenario", STUB_SCENARIOS)
def test_adapter_passes_the_shared_conformance_suite(
    gateway: ModelGateway, kind_value: str, scenario: str
) -> None:
    """Every adapter produces the same normalized stream for every scenario."""
    kind = KIND_IDS_BY_VALUE[kind_value]
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    report = run_scenario(gateway, model_catalog_id=model_id, scenario=scenario)
    assert report.provider_kind is kind
    assert report.model_catalog_id == model_id


def test_at_least_two_adapters_pass_the_same_suite(gateway: ModelGateway) -> None:
    """Acceptance: at least two provider adapters pass the same conformance suite."""
    passed: dict[str, list[str]] = {}
    for kind in STUB_PROVIDER_KINDS:
        model_id = catalog_model_for_kind(gateway.catalog, kind)
        passed[kind.value] = [scenario for scenario in STUB_SCENARIOS if _passes(gateway, model_id, scenario)]
    qualifying = {
        kind: scenarios for kind, scenarios in passed.items() if set(scenarios) == set(STUB_SCENARIOS)
    }
    assert len(qualifying) >= 2, f"fewer than two adapters passed the same suite: {passed}"


def test_every_adapter_sends_the_catalog_model_id(gateway: ModelGateway, stub: StubProvider) -> None:
    """The wire model id is the catalog's, never a source constant (D-018)."""
    seen: dict[str, str] = {}
    for kind in STUB_PROVIDER_KINDS:
        model_id = catalog_model_for_kind(gateway.catalog, kind)
        before = len(stub.state.calls)
        run_scenario(gateway, model_catalog_id=model_id, scenario="text")
        call = stub.state.calls[before]
        seen[kind.value] = str(call.json()["model"])
        assert seen[kind.value] == gateway.catalog.model(model_id).model_id
    assert len(set(seen.values())) == len(STUB_PROVIDER_KINDS)


def test_each_adapter_uses_its_own_wire_endpoint(gateway: ModelGateway, stub: StubProvider) -> None:
    paths = {}
    for kind in STUB_PROVIDER_KINDS:
        model_id = catalog_model_for_kind(gateway.catalog, kind)
        before = len(stub.state.calls)
        run_scenario(gateway, model_catalog_id=model_id, scenario="text")
        paths[kind.value] = stub.state.calls[before].path
    assert paths == {
        "anthropic": "/v1/messages",
        "openai": "/v1/chat/completions",
        "openai_compatible": "/v1/chat/completions",
    }


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_tool_call_is_strict_json_and_typed(gateway: ModelGateway, kind_value: str) -> None:
    kind = KIND_IDS_BY_VALUE[kind_value]
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    report = run_scenario(gateway, model_catalog_id=model_id, scenario="tool_call")
    tool_call = report.of_kind(intelligence_pb2.ModelEvent.KIND_TOOL_CALL)[0]
    assert tool_call.tool_name == "fs.read"
    assert json.loads(tool_call.tool_args_json) == {"value": "42"}
    stop = report.of_kind(intelligence_pb2.ModelEvent.KIND_STOP)[0]
    assert stop.stop_reason == intelligence_pb2.MODEL_STOP_REASON_TOOL_USE


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_usage_counts_cache_tokens_without_double_counting(gateway: ModelGateway, kind_value: str) -> None:
    """The provider reports usage twice; cumulative merge keeps the totals, not the sum."""
    kind = KIND_IDS_BY_VALUE[kind_value]
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    report = run_scenario(gateway, model_catalog_id=model_id, scenario="usage_repeat")
    usage = report.of_kind(intelligence_pb2.ModelEvent.KIND_USAGE)[0].usage
    assert (usage.input_tokens, usage.output_tokens, usage.cache_read_tokens) == (11, 7, 3)
    assert report.outcome is not None
    assert report.outcome.usage.input_tokens == 11
    assert report.outcome.usage.output_tokens == 7


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_refusal_is_a_typed_stop_reason(gateway: ModelGateway, kind_value: str) -> None:
    kind = KIND_IDS_BY_VALUE[kind_value]
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    report = run_scenario(gateway, model_catalog_id=model_id, scenario="refusal")
    assert (
        report.of_kind(intelligence_pb2.ModelEvent.KIND_STOP)[0].stop_reason
        == intelligence_pb2.MODEL_STOP_REASON_REFUSAL
    )
    assert report.of_kind(intelligence_pb2.ModelEvent.KIND_ERROR) == ()


def _passes(gateway: ModelGateway, model_id: str, scenario: str) -> bool:
    try:
        run_scenario(gateway, model_catalog_id=model_id, scenario=scenario)
    except AssertionError:
        return False
    return True
