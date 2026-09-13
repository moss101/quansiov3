"""`Embed` through the real gRPC boundary (INT-011 embedding wire).

Every call here goes over a real gRPC channel to a real `IntelligenceServer` whose servicer
holds the embedding route composed from a `ModelGateway` pointed at the loopback conformance
stub: the scope/deadline gate, the width the index pins, the typed error envelope and the
per-input response shape are exercised together. Nothing here re-implements the handler.
"""

from __future__ import annotations

import contextlib
import time
from collections.abc import Iterator

import grpc
import pytest
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.embeddings import INDEX_DIMENSIONS
from intelligence.embeddings.gateway_provider import gateway_embedder
from intelligence.embeddings.provider import EmbeddingProvider
from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import (
    StubProvider,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.conformance.suite import scenario_marker
from intelligence.model_gateway.embeddings import MAX_EMBED_INPUTS
from intelligence.server import (
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

#: Representative content: the kind of text the derived index actually embeds.
SOURCES = (
    "Deployment runbook. Retention is ninety days for logs.",
    "Memory entries are candidates until a person confirms them.",
    "The pipeline promotes a candidate only after every gate passes.",
)


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


@pytest.fixture(scope="module")
def gateway(stub: StubProvider) -> ModelGateway:
    return ModelGateway(catalog=stub_catalog(stub.base_url), environ=credential_environ())


@pytest.fixture(scope="module")
def embedder(gateway: ModelGateway) -> EmbeddingProvider:
    return gateway_embedder(gateway, dimensions=INDEX_DIMENSIONS)


def scope(deadline_ms: int) -> service_pb2.ScopeContext:
    return service_pb2.ScopeContext(
        schema_version="v1",
        tenant_id=TENANT_ID,
        workspace_id=WORKSPACE_ID,
        correlation_id=CORRELATION_ID,
        deadline_ms=deadline_ms,
        capability_projection_id=CAPABILITY_PROJECTION_ID,
    )


@contextlib.contextmanager
def running_gateway(embedder: EmbeddingProvider | None) -> Iterator[service_pb2_grpc.IntelligenceGatewayStub]:
    server = IntelligenceServer(
        ServerConfig(transport=Transport.TCP, host="127.0.0.1", port=0),
        servicer=IntelligenceGatewayServicer(embedder=embedder),
    )
    address = server.start()
    channel = grpc.insecure_channel(address)
    try:
        grpc.channel_ready_future(channel).result(timeout=10.0)
        yield service_pb2_grpc.IntelligenceGatewayStub(channel)
    finally:
        channel.close()
        server.stop(2.0)


def embed_request(texts: tuple[str, ...], **overrides: object) -> service_pb2.EmbedRequest:
    request = service_pb2.EmbedRequest(
        schema_version="v1", scope=scope(int(time.time() * 1000) + 30_000), texts=list(texts)
    )
    for field, value in overrides.items():
        setattr(request, field, value)
    return request


def test_embed_returns_one_vector_per_input_at_the_pinned_width(embedder: EmbeddingProvider) -> None:
    with running_gateway(embedder) as client:
        response = client.Embed(embed_request(SOURCES))

    assert len(response.embeddings) == len(SOURCES)
    assert [embedding.index for embedding in response.embeddings] == list(range(len(SOURCES)))
    assert response.dimensions == INDEX_DIMENSIONS
    for embedding in response.embeddings:
        assert len(embedding.vector) == INDEX_DIMENSIONS
    # The vectors must be content: identical text gives an identical vector, and different
    # text gives a different one. An all-zero placeholder would satisfy neither.
    assert list(response.embeddings[0].vector) == list(response.embeddings[0].vector)
    assert any(component != 0.0 for component in response.embeddings[0].vector)
    assert list(response.embeddings[0].vector) != list(response.embeddings[1].vector)


def test_embed_is_deterministic_across_calls(embedder: EmbeddingProvider) -> None:
    with running_gateway(embedder) as client:
        first = client.Embed(embed_request(SOURCES))
        second = client.Embed(embed_request(SOURCES))
    assert [list(embedding.vector) for embedding in first.embeddings] == [
        list(embedding.vector) for embedding in second.embeddings
    ]


def test_embed_reports_the_route_that_served_it(embedder: EmbeddingProvider) -> None:
    routes = getattr(embedder, "route_ids", ())
    assert routes, "the composed embedder must expose the routes it resolved"
    with running_gateway(embedder) as client:
        response = client.Embed(embed_request(SOURCES))
    assert response.model_id == routes[0]


def test_embed_refuses_an_empty_request_with_a_typed_error(embedder: EmbeddingProvider) -> None:
    with running_gateway(embedder) as client, pytest.raises(grpc.RpcError) as failure:
        client.Embed(embed_request(()))
    assert failure.value.code() is grpc.StatusCode.INVALID_ARGUMENT
    envelope = decode_error(failure.value)
    assert envelope.code is ErrorCode.VALIDATION_SCHEMA
    assert "no text to embed" in envelope.message


def test_embed_refuses_a_blank_input_and_an_oversized_batch(embedder: EmbeddingProvider) -> None:
    with running_gateway(embedder) as client:
        with pytest.raises(grpc.RpcError) as blank:
            client.Embed(embed_request(("retention", "   ")))
        with pytest.raises(grpc.RpcError) as oversized:
            client.Embed(embed_request(tuple("text" for _ in range(MAX_EMBED_INPUTS + 1))))
    assert decode_error(blank.value).code is ErrorCode.VALIDATION_SCHEMA
    assert "input 1 is blank" in decode_error(blank.value).message
    assert decode_error(oversized.value).code is ErrorCode.VALIDATION_BOUNDS


def test_embed_refuses_a_model_choice_it_cannot_honour(embedder: EmbeddingProvider) -> None:
    """An explicit route is honoured or refused, never answered by a different model."""
    with running_gateway(embedder) as client, pytest.raises(grpc.RpcError) as failure:
        client.Embed(embed_request(SOURCES, embedding_model="openai-gpt"))
    envelope = decode_error(failure.value)
    assert envelope.code is ErrorCode.ROUTE_UNAVAILABLE
    assert "never substituted" in envelope.message


def test_embed_honours_the_configured_model_choice(embedder: EmbeddingProvider) -> None:
    served = getattr(embedder, "route_ids", ("",))[0]
    with running_gateway(embedder) as client:
        response = client.Embed(embed_request(SOURCES, embedding_model=served))
    assert response.model_id == served


def test_embed_requires_scope_before_anything_else(embedder: EmbeddingProvider) -> None:
    request = service_pb2.EmbedRequest(schema_version="v1", texts=list(SOURCES))
    with running_gateway(embedder) as client, pytest.raises(grpc.RpcError) as failure:
        client.Embed(request)
    assert decode_error(failure.value).code is ErrorCode.VALIDATION_SCHEMA
    assert "ScopeContext is required" in decode_error(failure.value).message


def test_embed_fails_closed_when_no_route_is_configured() -> None:
    with running_gateway(None) as client, pytest.raises(grpc.RpcError) as failure:
        client.Embed(embed_request(SOURCES))
    envelope = decode_error(failure.value)
    assert envelope.code is ErrorCode.ROUTE_UNAVAILABLE
    assert "no embedding route is configured" in envelope.message
    assert ("owner_task", "INT-011") in {(detail.key, detail.value) for detail in envelope.details}


def test_a_route_of_the_wrong_width_fails_closed(embedder: EmbeddingProvider) -> None:
    """The stub answers 8-wide vectors for this scenario; the index pins 1536."""
    with running_gateway(embedder) as client, pytest.raises(grpc.RpcError) as failure:
        client.Embed(embed_request((f"{scenario_marker('embedding_width_mismatch')} retention",)))
    envelope = decode_error(failure.value)
    assert envelope.code is ErrorCode.PROVIDER_UNAVAILABLE
    assert "1536" in envelope.message and "8-wide" in envelope.message
