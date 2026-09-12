"""INT-003 data policy: a disallowed combination fails before transmission, and secrets are redacted.

Both suites drive the real gateway against the conformance stub provider, so "before
transmission" means the stub's own record of what arrived — not an internal flag.
"""

from __future__ import annotations

import json
from collections.abc import Iterator

import pytest
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import (
    StubProvider,
    build_call,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.dlp import DataClass, DlpGuard, DlpPolicy
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

SECRET = "sk-abcdefgh12345678"
EMAIL = "operator@example.com"


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


def gateway_for(stub: StubProvider, policy: DlpPolicy) -> ModelGateway:
    return ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ=credential_environ(),
        tool_schemas={},
        dlp=DlpGuard(policy),
    )


def call_with(text: str, *, profile: str = "") -> intelligence_pb2.ModelCallRequest:
    """A conformance call whose last message carries `text`."""
    request = build_call(model_catalog_id="", scenario="text")
    if profile:
        request.dlp_profile = profile
    # proto-plus returns a copy from a repeated field, so the message is replaced rather than
    # mutated in place.
    last = request.messages[-1]
    last.content_json = json.dumps(text)
    request.messages.pop()
    request.messages.append(last)
    return request


def test_a_disallowed_data_class_fails_before_transmission(stub: StubProvider) -> None:
    """Acceptance 2: nothing reaches the provider when the combination is not allowed."""
    providers = {config.name for config in stub_catalog(stub.base_url).providers.values()}
    strict = DlpPolicy(
        allowed_by_provider={name: frozenset({DataClass.PUBLIC}) for name in providers},
        data_class_by_profile={"strict": DataClass.CONFIDENTIAL},
    )
    gateway = gateway_for(stub, strict)
    before = len(stub.state.calls)

    with pytest.raises(GatewayError) as raised:
        list(gateway.fulfill(call_with("board pack", profile="strict")))
    assert raised.value.code == GatewayErrorCode.DLP_DENIED
    assert "confidential" in raised.value.message
    assert len(stub.state.calls) == before, "the denial happened before transmission"

    # The same content at an allowed class does transmit, so the denial was about policy, not shape.
    permitted = DlpPolicy(
        allowed_by_provider={name: frozenset({DataClass.PUBLIC, DataClass.INTERNAL}) for name in providers},
        data_class_by_profile={"strict": DataClass.CONFIDENTIAL},
    )
    open_gateway = gateway_for(stub, permitted)
    list(open_gateway.fulfill(call_with("public plan")))
    assert len(stub.state.calls) == before + 1


def test_secret_shaped_content_is_redacted_before_transmission(stub: StubProvider) -> None:
    """A secret never leaves: the bytes the provider received carry the placeholder instead."""
    providers = {config.name for config in stub_catalog(stub.base_url).providers.values()}
    policy = DlpPolicy(
        allowed_by_provider={name: frozenset({DataClass.PUBLIC, DataClass.INTERNAL}) for name in providers},
        # The redaction rules are policy data: the deployment's policy supplies them, so the test
        # supplies the shipped set rather than the guard inventing one.
        redactions=DlpPolicy.builtin().redactions,
    )
    gateway = gateway_for(stub, policy)

    before = len(stub.state.calls)
    list(gateway.fulfill(call_with(f"my key is {SECRET} and mail is {EMAIL}")))
    assert len(stub.state.calls) == before + 1, "the call went out"

    decision = gateway.dlp_decision
    assert decision is not None
    assert set(decision.fired_redactions) == {
        "dlp.redaction.secret_like",
        "dlp.redaction.email",
    }, f"both rules are recorded: {decision.describe()}"

    sent = stub.state.calls[-1].body.decode("utf-8", errors="replace")
    assert SECRET not in sent, "the secret never reaches the provider"
    assert EMAIL not in sent, "the address never reaches the provider"
    assert "[REDACTED:secret]" in sent and "[REDACTED:email]" in sent


def test_the_guard_refuses_a_model_that_is_not_cleared(stub: StubProvider) -> None:
    """A model's own clearance is checked as well as the provider's."""
    catalog = stub_catalog(stub.base_url)
    providers = {config.name for config in catalog.providers.values()}
    gateway = gateway_for(
        stub,
        DlpPolicy(allowed_by_provider={name: frozenset(DataClass) for name in providers}),
    )
    route = gateway.resolve_route(call_with("anything"))
    model = catalog.models[route.model.id]
    policy_model = type(model)(
        id=model.id,
        provider=model.provider,
        model_id=model.model_id,
        capabilities=model.capabilities,
        context_window=model.context_window,
        cost_class=model.cost_class,
        dlp_eligible=False,
    )
    with pytest.raises(GatewayError) as raised:
        DlpGuard(gateway.dlp_decision and DlpGuard().policy).check(
            call_with("anything"), policy_model, route.provider
        )
    assert raised.value.code == GatewayErrorCode.DLP_DENIED
    assert "dlp.model_eligibility" in raised.value.message
