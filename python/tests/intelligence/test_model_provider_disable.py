"""OPS-008 provider outage drill in the model plane: a disabled provider is never routed to.

The real gateway runs against the conformance stub provider, and what reaches the wire is
counted. A call whose selected provider an operator disabled fails over to the first catalog
fallback on an enabled provider. The remaining fallbacks exclude every disabled provider. A call
with no enabled provider left is refused before any request is sent.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import (
    StubProvider,
    build_call,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.gateway import RULE_OPERATOR_FAILOVER


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


@pytest.fixture(scope="module")
def gateway(stub: StubProvider) -> ModelGateway:
    return ModelGateway(catalog=stub_catalog(stub.base_url), environ=credential_environ(), tool_schemas={})


def test_a_disabled_provider_fails_over_and_is_dropped_from_the_fallbacks(
    gateway: ModelGateway, stub: StubProvider
) -> None:
    request = build_call(model_catalog_id="", scenario="text")
    normal = gateway.resolve_route(request)
    down = normal.provider.name

    request.disabled_providers.append(down)
    rerouted = gateway.resolve_route(request)
    assert rerouted.provider.name != down, "the disabled provider is never selected"
    assert rerouted.route.chosen_by == RULE_OPERATOR_FAILOVER, "the route records why it moved"
    for model_id in rerouted.route.fallbacks:
        assert gateway.catalog.provider_for(gateway.catalog.models[model_id]).name != down

    # End to end: the only provider request on the wire goes to the enabled provider's model.
    before = len(stub.state.calls)
    for _ in gateway.fulfill(request):
        pass
    assert len(stub.state.calls) == before + 1
    assert rerouted.model.model_id in str(stub.state.calls[-1].json())
    assert normal.model.model_id not in str(stub.state.calls[-1].json())


def test_with_every_provider_disabled_nothing_is_sent(gateway: ModelGateway, stub: StubProvider) -> None:
    request = build_call(model_catalog_id="", scenario="text")
    for provider in gateway.catalog.providers.values():
        request.disabled_providers.append(provider.name)
    before = len(stub.state.calls)
    with pytest.raises(GatewayError) as refused:
        gateway.resolve_route(request)
    assert refused.value.code == GatewayErrorCode.PROVIDER_UNAVAILABLE
    assert refused.value.retryable is False
    with pytest.raises(GatewayError):
        for _ in gateway.fulfill(request):
            pass
    assert len(stub.state.calls) == before, "a refused call never reaches a provider"


def test_without_disabled_providers_routing_is_unchanged(gateway: ModelGateway) -> None:
    request = build_call(model_catalog_id="", scenario="text")
    first = gateway.resolve_route(request)
    request.disabled_providers.append("a-provider-not-in-the-catalog")
    second = gateway.resolve_route(request)
    assert (first.model.id, list(first.route.fallbacks)) == (second.model.id, list(second.route.fallbacks))
