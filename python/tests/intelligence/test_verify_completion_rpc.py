"""`VerifyCompletion` through the real gRPC boundary (RUN-008 semantic verifier + INT-002 gateway).

Every call goes over a real gRPC channel to a real `IntelligenceServer` whose servicer holds a
`ModelGateway` over the repository catalog, pointed at the loopback conformance stub. The stub's
`verdict_*` scenarios answer with a strict verdict; its other scenarios prove that anything else
is a typed refusal, never agreement.

Real boundary: none (the conformance stub provider is loopback and deterministic).
"""

from __future__ import annotations

import contextlib
import time
from collections.abc import Iterator
from typing import Any

import grpc
import pytest
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.evaluation.semantic_verifier import (
    INDEPENDENCE_UNPROVABLE,
    VERDICT_MALFORMED,
    VERIFIER_UNAVAILABLE,
)
from intelligence.model_gateway import GatewayError, ModelGateway
from intelligence.model_gateway.catalog import ModelCatalog
from intelligence.model_gateway.conformance import StubProvider, credential_environ, stub_catalog
from intelligence.server import (
    ErrorCode,
    IntelligenceGatewayServicer,
    IntelligenceServer,
    ServerConfig,
    Transport,
    decode_error,
)

TENANT_ID = "tn_01J8Z3K6F1N8VQ2X5W9Y0RR008"
#: The claimant is the catalog's primary model (the verification model differs from it).
CLAIMANT = ModelCatalog.load().primary().model_id
WORKSPACE_ID = "ws_01J8Z3K6F1N8VQ2X5W9Y0RR008"


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


@pytest.fixture(scope="module")
def gateway(stub: StubProvider) -> ModelGateway:
    return ModelGateway(catalog=stub_catalog(stub.base_url), environ=credential_environ(), tool_schemas={})


@contextlib.contextmanager
def serving(gateway: ModelGateway | None) -> Iterator[Any]:
    server = IntelligenceServer(
        ServerConfig(transport=Transport.TCP, host="127.0.0.1", port=0),
        servicer=IntelligenceGatewayServicer(gateway=gateway),
    )
    address = server.start()
    channel = grpc.insecure_channel(address)
    try:
        grpc.channel_ready_future(channel).result(timeout=10.0)
        yield service_pb2_grpc.IntelligenceGatewayStub(channel)
    finally:
        channel.close()
        server.stop(2.0)


def request(summary: str, **overrides: Any) -> service_pb2.CompletionVerificationRequest:
    message = service_pb2.CompletionVerificationRequest(
        schema_version="v1",
        scope=service_pb2.ScopeContext(
            schema_version="v1",
            tenant_id=TENANT_ID,
            workspace_id=WORKSPACE_ID,
            correlation_id="run_x:verify",
            deadline_ms=int(time.time() * 1000) + 30_000,
            capability_projection_id="runtime:verification",
        ),
        run_id="run_01J8Z3K6F1N8VQ2X5W9Y0RR008",
        work_node_id="wn_01J8Z3K6F1N8VQ2X5W9Y0RR008",
        claim_summary=summary,
        rubric_id="rubric_report",
        claimant_model=CLAIMANT,
        max_output_tokens=256,
    )
    for field, value in overrides.items():
        setattr(message, field, value)
    return message


def refusal(error: grpc.RpcError) -> tuple[ErrorCode, dict[str, str]]:
    envelope = decode_error(error)
    return envelope.code, {detail.key: detail.value for detail in envelope.details}


def test_an_agreeing_verdict_comes_from_the_catalog_verification_model(
    gateway: ModelGateway, stub: StubProvider
) -> None:
    before = len(stub.state.calls)
    with serving(gateway) as client:
        verdict = client.VerifyCompletion(request("the report is written [[scenario:verdict_agree]]"))
    assert verdict.agrees is True
    assert verdict.critique == "the cited evidence shows the work done"
    verifier = gateway.catalog.verifier()
    assert verdict.model == verifier.id == gateway.catalog.routing_verification
    assert verifier.id != gateway.catalog.routing_primary
    assert len(stub.state.calls) == before + 1, "the verdict came from a real provider call"
    assert verifier.model_id.encode() in (stub.state.calls[-1].body or b""), "routed to the verifier"


def test_a_disagreeing_verdict_is_returned_with_its_critique(gateway: ModelGateway) -> None:
    with serving(gateway) as client:
        verdict = client.VerifyCompletion(request("done [[scenario:verdict_disagree]]"))
    assert verdict.agrees is False
    assert verdict.critique == "the evidence does not show the report"


def test_an_answer_that_is_not_a_verdict_is_a_typed_refusal(gateway: ModelGateway) -> None:
    with serving(gateway) as client, pytest.raises(grpc.RpcError) as raised:
        client.VerifyCompletion(request("done"))
    code, details = refusal(raised.value)
    assert code is ErrorCode.VALIDATION_SCHEMA
    assert details == {"refusal": VERDICT_MALFORMED, "owner_task": "RUN-008"}


@pytest.mark.parametrize("named", ["unnamed", "wire_id", "catalog_id"])
def test_independence_it_cannot_prove_is_refused_before_any_model_call(
    gateway: ModelGateway, stub: StubProvider, named: str
) -> None:
    verifier = gateway.catalog.verifier()
    claimant = {"unnamed": "", "wire_id": verifier.model_id, "catalog_id": verifier.id}[named]
    before = len(stub.state.calls)
    with serving(gateway) as client, pytest.raises(grpc.RpcError) as raised:
        client.VerifyCompletion(
            request("done [[scenario:verdict_agree]]", independent_model=True, claimant_model=claimant)
        )
    code, details = refusal(raised.value)
    assert raised.value.code() == grpc.StatusCode.PERMISSION_DENIED
    assert code is ErrorCode.POLICY_DENIED
    assert details["refusal"] == INDEPENDENCE_UNPROVABLE
    assert len(stub.state.calls) == before, "no model judged its own work"


def test_an_independent_claimant_is_judged(gateway: ModelGateway) -> None:
    with serving(gateway) as client:
        verdict = client.VerifyCompletion(request("done [[scenario:verdict_agree]]", independent_model=True))
    assert verdict.agrees is True


def test_a_provider_failure_mid_stream_is_an_unavailable_verifier(gateway: ModelGateway) -> None:
    with serving(gateway) as client, pytest.raises(grpc.RpcError) as raised:
        client.VerifyCompletion(request("done [[scenario:stream_error]]"))
    code, details = refusal(raised.value)
    assert code is ErrorCode.PROVIDER_UNAVAILABLE
    assert details["refusal"] == VERIFIER_UNAVAILABLE


def test_without_a_gateway_the_verifier_is_unavailable() -> None:
    with serving(None) as client, pytest.raises(grpc.RpcError) as raised:
        client.VerifyCompletion(request("done [[scenario:verdict_agree]]"))
    code, details = refusal(raised.value)
    assert code is ErrorCode.PROVIDER_UNAVAILABLE
    assert details == {"owner_task": "RUN-008"}


def test_the_verification_route_is_a_catalog_model_and_defaults_to_the_primary() -> None:
    base: dict[str, Any] = {
        "version": 1,
        "providers": {"p": {"kind": "openai_compatible", "base_url": "http://127.0.0.1:1/v1"}},
        "routing": {"strategy": "deterministic", "primary": "a"},
        "models": [
            {
                "id": model,
                "provider": "p",
                "model_id": f"{model}-wire",
                "capabilities": ["streaming"],
                "context_window": 1000,
                "cost_class": "low",
                "dlp_eligible": False,
            }
            for model in ("a", "b")
        ],
    }
    assert ModelCatalog.from_mapping(base).verifier().id == "a"
    base["routing"]["verification"] = "b"
    assert ModelCatalog.from_mapping(base).verifier().id == "b"
    base["routing"]["verification"] = "missing"
    with pytest.raises(GatewayError):
        ModelCatalog.from_mapping(base)
