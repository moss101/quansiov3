"""Live proof of routing, DLP and failover (INT-003) — the real boundary.

INT-003's selector is pure and its guard decides before a byte is built, so the offline suites
(`test_model_routing.py`, `test_model_dlp.py`) prove the policy against the stub. The task is
`real_boundary: true` because its provider path is INT-002's: whether the route it chooses is
the one that is called, whether a denial really keeps a request off the wire, and whether
failover really lands on another provider are only shown against a live one.

This module reads the same `QUANSIO_TEST_*` variables as `test_model_gateway_live.py` and
nothing else; each case skips with an explicit `BLOCKED_EXTERNAL` marker when no live
provider is configured. The transport is the real one, wrapped only to count what it opens.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from dataclasses import dataclass, field

import pytest
import yaml
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.catalog import ModelCatalog, ProviderKind, default_catalog_path
from intelligence.model_gateway.conformance import LIVE_PROVIDER_KINDS, build_call
from intelligence.model_gateway.credentials import env_var_for_handle
from intelligence.model_gateway.dlp import DataClass, DlpGuard, DlpPolicy
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.routing import PolicyRouteSelector, bounded_failover
from intelligence.model_gateway.transport import HttpRequest, StdlibHttpTransport, StreamResponse
from tests.intelligence.test_model_gateway_live import LIVE_TIMEOUT_MS, LiveBoundary, live_boundary

pytestmark = pytest.mark.live

#: A closed loopback port: the connection is refused before any provider is involved.
DEAD_ENDPOINT = "http://127.0.0.1:9"
DEAD_CREDENTIAL_HANDLE = "provider/dead"
CALL_ID = "call_01J8Z3K6F1N8VQ2X5W9Y0LIVE3"


@dataclass(slots=True)
class CountingTransport:
    """The real transport, recording the URL of every connection it opens."""

    inner: StdlibHttpTransport = field(default_factory=StdlibHttpTransport)
    opened: list[str] = field(default_factory=list)

    def open(self, request: HttpRequest) -> StreamResponse:
        self.opened.append(request.url)
        return self.inner.open(request)


def configured_boundaries() -> Iterator[LiveBoundary]:
    for kind in LIVE_PROVIDER_KINDS:
        try:
            yield live_boundary(kind)
        except LookupError:
            continue


def any_live_boundary() -> LiveBoundary:
    """One configured live provider, or `BLOCKED_EXTERNAL` naming what would configure one."""
    for boundary in configured_boundaries():
        return boundary
    pytest.skip(
        "BLOCKED_EXTERNAL: no live provider is configured; set the QUANSIO_TEST_* variables of "
        "any one provider (see test_model_gateway_live.py)"
    )


def live_provider_name(boundary: LiveBoundary) -> str:
    return boundary.catalog.provider_for(boundary.catalog.model(boundary.model_id)).name


def live_gateway(boundary: LiveBoundary, *, policy: DlpPolicy, transport: CountingTransport) -> ModelGateway:
    """INT-003's selector and guard on the live catalog, over the counting transport."""
    return ModelGateway(
        catalog=boundary.catalog,
        environ=boundary.environ,
        tool_schemas={},
        transport=transport,
        selector=PolicyRouteSelector(),
        dlp=DlpGuard(policy),
    )


def clearance(boundary: LiveBoundary, *classes: DataClass) -> DlpPolicy:
    """A policy that clears the live provider for exactly `classes`."""
    return DlpPolicy(
        allowed_by_provider={live_provider_name(boundary): frozenset(classes)},
        data_class_by_profile={"strict": DataClass.CONFIDENTIAL},
    )


def outcome_of(gateway: ModelGateway, request: intelligence_pb2.ModelCallRequest):
    fulfillment = gateway.fulfill(request)
    events = list(fulfillment)
    return fulfillment.outcome, events


def test_live_route_is_deterministic_and_is_the_only_model_called() -> None:
    """Acceptance 1, live: the route resolves without I/O, and the call reaches it once."""
    boundary = any_live_boundary()
    transport = CountingTransport()
    gateway = live_gateway(
        boundary, policy=clearance(boundary, DataClass.PUBLIC, DataClass.INTERNAL), transport=transport
    )
    request = build_call(
        model_catalog_id=boundary.model_id, scenario="text", timeout_ms=LIVE_TIMEOUT_MS, call_id=CALL_ID
    )

    first = gateway.resolve_route(request)
    second = gateway.resolve_route(request)
    assert (first.model.id, first.route.id, first.route.chosen_by) == (
        second.model.id,
        second.route.id,
        second.route.chosen_by,
    )
    assert first.route.chosen_by, "every decision names the rule that decided it"
    assert transport.opened == [], "resolving a route opens no connection: no auxiliary model call"

    outcome, _events = outcome_of(gateway, request)
    assert outcome is not None and outcome.terminal and outcome.error_code is None
    assert outcome.stop_reason == intelligence_pb2.MODEL_STOP_REASON_END_TURN
    assert outcome.usage.input_tokens > 0 and outcome.usage.output_tokens > 0
    assert len(transport.opened) == 1, "exactly one provider call, and nothing before it"
    endpoint = boundary.catalog.provider_for(first.model).resolve_base_url(boundary.environ)
    assert transport.opened[0].startswith(endpoint), "the one call went to the selected route's provider"


def test_live_dlp_denial_keeps_the_request_off_the_wire() -> None:
    """Acceptance 2, live: a disallowed data/provider combination opens no connection at all."""
    boundary = any_live_boundary()
    transport = CountingTransport()
    strict = live_gateway(
        boundary, policy=clearance(boundary, DataClass.PUBLIC, DataClass.INTERNAL), transport=transport
    )
    request = build_call(
        model_catalog_id=boundary.model_id, scenario="text", timeout_ms=LIVE_TIMEOUT_MS, call_id=CALL_ID
    )
    request.dlp_profile = "strict"

    with pytest.raises(GatewayError) as raised:
        outcome_of(strict, request)
    assert raised.value.code is GatewayErrorCode.DLP_DENIED
    assert transport.opened == [], "the denial happened before transmission"

    # The same content at the default class transmits, so the denial was policy, not shape.
    request.dlp_profile = ""
    outcome, _ = outcome_of(strict, request)
    assert outcome is not None and outcome.error_code is None
    assert len(transport.opened) == 1


def failover_catalog(boundary: LiveBoundary) -> tuple[ModelCatalog, dict[str, str], str]:
    """The live catalog with a dead first-party primary ahead of the live route as sole fallback.

    Returns the catalog, its environ (the live credential plus a placeholder for the dead
    provider, which is never authenticated because the connection is refused first) and the
    dead model's catalog id.
    """
    data = yaml.safe_load(default_catalog_path().read_text(encoding="utf-8"))
    live_spec = boundary.catalog.model(boundary.model_id)
    live_provider = boundary.catalog.provider_for(live_spec)
    # Carry the live boundary's resolved endpoint and model into the mapping verbatim.
    provider_entry = data["providers"][live_provider.name]
    provider_entry.pop("base_url_env", None)
    provider_entry["base_url"] = live_provider.resolve_base_url(boundary.environ)
    for entry in data["models"]:
        if entry["id"] == live_spec.id:
            entry["model_id"] = live_spec.model_id
    dead_kind = ProviderKind.OPENAI if live_spec.provider != "openai" else ProviderKind.ANTHROPIC
    dead_provider_name = next(
        name for name, entry in data["providers"].items() if entry["kind"] == dead_kind.value
    )
    dead_entry = data["providers"][dead_provider_name]
    dead_entry.pop("base_url_env", None)
    dead_entry.pop("credential_handle_env", None)
    dead_entry["base_url"] = DEAD_ENDPOINT
    dead_entry["credential_handle"] = DEAD_CREDENTIAL_HANDLE
    dead_model = next(
        entry["id"]
        for entry in data["models"]
        if entry["provider"] == dead_provider_name and "embeddings" not in entry["capabilities"]
    )
    # Only the two routes exist, so the selector's bounded alternatives can be nothing else.
    data["models"] = [entry for entry in data["models"] if entry["id"] in {dead_model, live_spec.id}]
    data["routing"]["primary"] = dead_model
    data["routing"]["fallback_order"] = [live_spec.id]
    catalog = ModelCatalog.from_mapping(
        data, source=f"{default_catalog_path()} (live failover: {dead_model} -> {live_spec.id})"
    )
    environ = {**boundary.environ, env_var_for_handle(DEAD_CREDENTIAL_HANDLE): "dead-provider-placeholder"}
    return catalog, environ, dead_model


def test_live_failover_lands_on_the_live_provider_with_the_same_request() -> None:
    """Failover, live: a refused primary is left behind, bounded, and the live fallback answers
    the very same request identity."""
    boundary = any_live_boundary()
    catalog, environ, dead_model = failover_catalog(boundary)
    transport = CountingTransport()
    gateway = ModelGateway(
        catalog=catalog,
        environ=environ,
        tool_schemas={},
        transport=transport,
        selector=PolicyRouteSelector(),
        dlp=DlpGuard(
            DlpPolicy(
                allowed_by_provider={
                    name: frozenset({DataClass.PUBLIC, DataClass.INTERNAL}) for name in catalog.providers
                }
            )
        ),
    )
    request = build_call(model_catalog_id="", scenario="text", timeout_ms=LIVE_TIMEOUT_MS, call_id=CALL_ID)
    decision = gateway.resolve_route(request)
    assert decision.model.id == dead_model, "the catalog primary is the dead route"
    assert list(decision.route.fallbacks) == [boundary.model_id]

    attempted: list[str] = []

    def attempt(model) -> intelligence_pb2.ModelEvent:
        attempted.append(model.id)
        request.route_hint = model.id  # the caller's own request, re-aimed; never a new one
        outcome, _ = outcome_of(gateway, request)
        assert outcome is not None and outcome.call_id == CALL_ID
        if outcome.error_code is not None:
            raise GatewayError(
                GatewayErrorCode(outcome.error_code),
                f"attempt on {model.id} failed",
                retryable=outcome.retryable,
            )
        return outcome

    outcome, served_by = bounded_failover(
        decision,
        catalog,
        attempt,
        is_retryable=lambda error: isinstance(error, GatewayError) and error.retryable,
        max_attempts=2,
    )
    assert attempted == [dead_model, boundary.model_id], "primary refused, then exactly the live fallback"
    assert served_by.id == boundary.model_id
    assert outcome.call_id == CALL_ID and outcome.error_code is None and outcome.terminal
    assert outcome.usage.output_tokens > 0
    assert transport.opened[0].startswith(DEAD_ENDPOINT)
    live_endpoint = catalog.provider_for(served_by).resolve_base_url(environ)
    assert transport.opened[1].startswith(live_endpoint)
    assert len(transport.opened) == 2, "bounded: one attempt per route, no retry storm"
    assert json.loads(request.messages[-1].content_json).startswith("[[scenario:text]]"), (
        "the request content was not rebuilt"
    )
