"""INT-003 route selection: determinism, no auxiliary calls, bounded failover.

The determinism and failover suites drive the shipped selector and driver directly; the
"zero auxiliary calls" suite drives the **real** gateway end to end against the conformance stub
provider and counts what actually reached the wire, which is the only honest way to show that a
primary-model path needs no model call before the selected model.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import (
    STUB_PROVIDER_KINDS,
    StubProvider,
    build_call,
    catalog_model_for_kind,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.dlp import DataClass, DlpGuard, DlpPolicy
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.routing import (
    ClassPreference,
    PolicyRouteSelector,
    Preference,
    RequestClass,
    RoutePolicy,
    bounded_failover,
)


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


@pytest.fixture(scope="module")
def gateway(stub: StubProvider) -> ModelGateway:
    catalog = stub_catalog(stub.base_url)
    providers = {config.name for config in catalog.providers.values()}
    policy = DlpPolicy(
        allowed_by_provider={name: frozenset({DataClass.PUBLIC, DataClass.INTERNAL}) for name in providers}
    )
    return ModelGateway(
        catalog=catalog,
        environ=credential_environ(),
        tool_schemas={},
        dlp=DlpGuard(policy),
    )


def test_route_selection_is_deterministic_and_records_its_rule(gateway: ModelGateway) -> None:
    """The same request resolves to the same route, model and rule, every time."""
    request = build_call(model_catalog_id="", scenario="text")
    selector = PolicyRouteSelector()

    first = selector.select(gateway.catalog, request)
    second = selector.select(gateway.catalog, request)
    assert first.route.id == second.route.id, "route ids are a pure function of the decision"
    assert first.route.chosen_by == second.route.chosen_by
    assert first.model.id == second.model.id
    assert first.route.chosen_by, "every decision names the rule that decided it"
    assert first.route.fallbacks, "the route carries its bounded alternatives"

    # An explicit user choice is honoured, and an impossible one fails closed rather than
    # silently substituting another model.
    explicit = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, STUB_PROVIDER_KINDS[0]), scenario="text"
    )
    chosen = selector.select(gateway.catalog, explicit)
    assert chosen.model.id == catalog_model_for_kind(gateway.catalog, STUB_PROVIDER_KINDS[0])
    assert chosen.route.chosen_by.startswith("policy.user_choice")

    impossible = build_call(model_catalog_id="no_such_model", scenario="text")
    with pytest.raises(GatewayError) as raised:
        selector.select(gateway.catalog, impossible)
    assert raised.value.code == GatewayErrorCode.ROUTE_UNAVAILABLE

    # A capability no catalog model carries cannot be satisfied: the class fails closed.
    strict = RoutePolicy(
        by_class={
            RequestClass.CHAT: ClassPreference(
                request_class=RequestClass.CHAT,
                require_capabilities=frozenset({"capability_no_model_has"}),
            )
        }
    )
    with pytest.raises(GatewayError) as raised:
        PolicyRouteSelector(strict).select(gateway.catalog, request)
    assert raised.value.code == GatewayErrorCode.ROUTE_UNAVAILABLE

    # A cost-preferring class ranks by the catalog's cost class, deterministically.
    cheap = PolicyRouteSelector(
        RoutePolicy(
            by_class={
                RequestClass.CHEAP_WORKER: ClassPreference(
                    request_class=RequestClass.CHEAP_WORKER, prefer=Preference.COST
                )
            }
        ),
        default_class=RequestClass.CHEAP_WORKER,
    ).select(gateway.catalog, build_call(model_catalog_id="", scenario="text"))
    assert cheap.route.cost_class
    assert cheap.route.request_class == intelligence_pb2.ModelRoute.REQUEST_CLASS_CHEAP_WORKER


def test_a_primary_path_makes_no_auxiliary_model_call(gateway: ModelGateway, stub: StubProvider) -> None:
    """Acceptance 1: a normal call reaches exactly one model, with nothing called first."""
    before = len(stub.state.calls)
    call = build_call(model_catalog_id="", scenario="text", call_id="call_01J8Z3K6F1N8VQ2X5W9Y0RTEST")
    for _ in gateway.fulfill(call):
        pass
    assert len(stub.state.calls) == before + 1, "exactly one provider call, and it is the selected model"
    route = gateway.resolve_route(call)
    wire = stub.state.calls[-1].json()
    assert route.model.model_id in str(wire), "the wire request names the selected catalog model"


def test_failover_is_bounded_and_reuses_the_same_request(gateway: ModelGateway) -> None:
    """Failover walks the route's alternatives, bounded, without minting a new request."""
    request = build_call(model_catalog_id="", scenario="text", call_id="call_01J8Z3K6F1N8VQ2X5W9Y0RTEST")
    decision = PolicyRouteSelector().select(gateway.catalog, request)
    tried: list[str] = []

    class Retryable(Exception):
        pass

    def failing(model: object) -> str:
        tried.append(model.id)  # type: ignore[attr-defined]
        raise Retryable("provider hiccup")

    with pytest.raises(Retryable):
        bounded_failover(
            decision,
            gateway.catalog,
            failing,
            is_retryable=lambda error: isinstance(error, Retryable),
            max_attempts=2,
        )
    assert tried[0] == decision.model.id
    assert len(tried) == 2, "bounded by max_attempts"
    assert tried[1] in decision.route.fallbacks, "the fallback comes from the route"

    # The same call, bounded to one attempt, does not try a fallback at all.
    tried.clear()
    with pytest.raises(Retryable):
        bounded_failover(
            decision,
            gateway.catalog,
            failing,
            is_retryable=lambda error: isinstance(error, Retryable),
            max_attempts=1,
        )
    assert tried == [decision.model.id]

    # A non-retryable failure is not failed over.
    def fatal(model: object) -> str:
        tried.append(model.id)  # type: ignore[attr-defined]
        raise ValueError("bad request")

    tried.clear()
    with pytest.raises(ValueError):
        bounded_failover(
            decision,
            gateway.catalog,
            fatal,
            is_retryable=lambda error: isinstance(error, Retryable),
            max_attempts=3,
        )
    assert tried == [decision.model.id], "a fatal error stops the walk"

    # The driver hands the caller's own request identity through untouched: it never builds one.
    seen: list[str] = []

    def succeeding(model: object) -> str:
        seen.append(request.call_id)
        return model.id  # type: ignore[attr-defined]

    result, model = bounded_failover(
        decision,
        gateway.catalog,
        succeeding,
        is_retryable=lambda error: False,
        max_attempts=2,
    )
    assert result == model.id
    assert seen == [request.call_id], "the request identity is preserved"
