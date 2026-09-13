"""Embedding fulfilment for the one gateway (INT-011, DOMAIN.md §11.1).

`FulfillModel` streams chat-shaped events; an embedding call is not a stream, so the
`embedding` request class is answered by a *non-streaming* fulfilment. It is not a second path
to a provider: the gateway still resolves the route deterministically, materializes the
credential, enforces the DLP guard before a byte is built and sends over the pinned transport.
Only the response shape differs (vectors instead of `ModelEvent`s), which is why this module
owns a wire format rather than a runtime.

Two properties are the derived index's, and are enforced here because the wire is where they
can be observed:

* **the declared width is the contract.** A route states its `embedding_dimensions` in
  `config/models.yaml`; a response of another width is refused rather than written, so a
  failover can never cross embedding spaces.
* **one vector per input, in input order.** The provider's own `index` field is honoured
  instead of trusting array order, and a set that does not cover every input exactly once is
  refused rather than partially indexed.
"""

from __future__ import annotations

import json
import math
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Protocol

from intelligence.model_gateway.adapters.base import (
    PROVIDER_FEATURES_DISABLED,
    PreparedRequest,
    assert_no_disabled_features,
)
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.credentials import SecretValue
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

#: OpenAI-compatible embeddings endpoint. The version segment lives in the path, like the
#: chat-completions path, because a catalog `base_url` is an endpoint root.
EMBEDDINGS_PATH = "/v1/embeddings"

#: Rule ids, named so a caller can tell which refusal fired.
RULE_WIRE = "embedding.wire"
RULE_EMPTY_INPUT = "embedding.empty_input"
RULE_INPUT_BOUNDS = "embedding.input_bounds"
RULE_RESPONSE_SHAPE = "embedding.response_shape"
RULE_RESPONSE_WIDTH = "embedding.response_width"

#: Largest batch one embedding call may carry; a larger one is refused, not split silently.
MAX_EMBED_INPUTS = 256

#: Ceiling on the response bytes one embedding call may read before it is refused.
EMBEDDING_RESPONSE_MAX_BYTES = 8 * 1024 * 1024


def response_byte_budget(inputs: int, dimensions: int) -> int:
    """Bytes a legitimate response for this request can need: vectors plus framing.

    A provider that answers with something far larger is refused rather than buffered, so a
    malformed or hostile endpoint cannot exhaust the process.
    """
    per_vector = dimensions * 24
    return min(EMBEDDING_RESPONSE_MAX_BYTES, 64 * 1024 + inputs * per_vector)


@dataclass(frozen=True, slots=True)
class EmbeddingCallContext:
    """Everything an embedding wire may use to build one provider request."""

    model_catalog_id: str
    wire_model_id: str
    provider: str
    credential: SecretValue
    base_url: str
    texts: tuple[str, ...]
    timeout_seconds: float


class EmbeddingWire(Protocol):
    """One provider's embedding wire format."""

    kind: ProviderKind

    def build_request(self, context: EmbeddingCallContext) -> PreparedRequest: ...

    def decode(self, prepared: PreparedRequest, body: bytes) -> tuple[tuple[float, ...], ...]:
        """One vector per input, in input order; anything else is refused."""


class OpenAIEmbeddingWire:
    """OpenAI embeddings (`POST {base_url}/embeddings`); the compatible profile reuses it."""

    kind = ProviderKind.OPENAI

    __slots__ = ()

    def build_request(self, context: EmbeddingCallContext) -> PreparedRequest:
        body = {"model": context.wire_model_id, "input": list(context.texts)}
        payload = json.dumps(body, sort_keys=True, separators=(",", ":")).encode("utf-8")
        prepared = PreparedRequest(
            provider=context.provider,
            provider_kind=self.kind,
            model_catalog_id=context.model_catalog_id,
            url=f"{context.base_url}{EMBEDDINGS_PATH}",
            headers={
                "content-type": "application/json",
                "accept": "application/json",
                "authorization": context.credential.with_scheme("Bearer"),
                "user-agent": "quansio-intelligence-model-gateway",
            },
            body=payload,
            timeout_seconds=context.timeout_seconds,
            disabled_features=PROVIDER_FEATURES_DISABLED,
        )
        assert_no_disabled_features(prepared.body, prepared.headers)
        return prepared

    def decode(self, prepared: PreparedRequest, body: bytes) -> tuple[tuple[float, ...], ...]:
        return parse_embedding_response(prepared, body)


class OpenAICompatibleEmbeddingWire(OpenAIEmbeddingWire):
    """The generic endpoint profile: the same endpoint and payload, a self-hosted base URL."""

    kind = ProviderKind.OPENAI_COMPATIBLE

    __slots__ = ()


EMBEDDING_WIRES: tuple[type[EmbeddingWire], ...] = (OpenAIEmbeddingWire, OpenAICompatibleEmbeddingWire)


def embedding_wire_for_kind(kind: ProviderKind) -> EmbeddingWire:
    """The embedding wire registered for a provider kind; a kind without one fails closed.

    Anthropic publishes no embeddings endpoint, so an embedding route that resolves to it is a
    routing error rather than something to paper over with a chat call.
    """
    for wire_class in EMBEDDING_WIRES:
        if wire_class.kind == kind:
            return wire_class()
    raise GatewayError(
        GatewayErrorCode.ROUTE_UNAVAILABLE,
        f"provider kind {kind} has no embeddings endpoint (rule {RULE_WIRE})",
    )


def validate_embedding_inputs(texts: Sequence[str]) -> tuple[str, ...]:
    """Bounds every embedding request: non-empty, bounded count, no blank entry."""
    if not texts:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"no text to embed (rule {RULE_EMPTY_INPUT})")
    if len(texts) > MAX_EMBED_INPUTS:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_BOUNDS,
            f"{len(texts)} inputs exceed the {MAX_EMBED_INPUTS}-input embedding bound "
            f"(rule {RULE_INPUT_BOUNDS})",
        )
    for index, text in enumerate(texts):
        if not text.strip():
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"input {index} is blank (rule {RULE_EMPTY_INPUT})",
            )
    return tuple(texts)


def parse_embedding_response(prepared: PreparedRequest, body: bytes) -> tuple[tuple[float, ...], ...]:
    """Decode a provider embedding response strictly, one vector per input in input order."""
    try:
        decoded = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise GatewayError(
            GatewayErrorCode.PROVIDER_UNAVAILABLE,
            f"provider sent a malformed embeddings payload (rule {RULE_RESPONSE_SHAPE})",
        ) from error
    if not isinstance(decoded, dict):
        raise GatewayError(
            GatewayErrorCode.PROVIDER_UNAVAILABLE,
            f"provider sent a non-object embeddings payload (rule {RULE_RESPONSE_SHAPE})",
        )
    error_payload = decoded.get("error")
    if error_payload is not None:
        raise _error_from_payload(error_payload)
    entries = decoded.get("data")
    if not isinstance(entries, list) or not entries:
        raise GatewayError(
            GatewayErrorCode.PROVIDER_UNAVAILABLE,
            f"embeddings payload carries no data (rule {RULE_RESPONSE_SHAPE})",
        )
    expected = _expected_inputs(prepared)
    vectors: list[tuple[float, ...] | None] = [None] * expected
    for entry in entries:
        if not isinstance(entry, dict):
            raise _shape_error("an embedding entry is not an object")
        index = entry.get("index")
        if not isinstance(index, int) or isinstance(index, bool) or not 0 <= index < expected:
            raise _shape_error(f"embedding index {index!r} is outside 0..{expected - 1}")
        if vectors[index] is not None:
            raise _shape_error(f"embedding index {index} appears twice")
        vectors[index] = _vector(entry.get("embedding"), index)
    if any(vector is None for vector in vectors):
        raise _shape_error(f"the payload covers {entries.__len__()} of {expected} inputs")
    return tuple(vector for vector in vectors if vector is not None)


def _expected_inputs(prepared: PreparedRequest) -> int:
    """How many vectors the request asked for: the transmitted body is the authority."""
    inputs = prepared.body_json().get("input")
    if not isinstance(inputs, list) or not inputs:
        raise GatewayError(
            GatewayErrorCode.INTERNAL, f"embedding request carried no input (rule {RULE_WIRE})"
        )
    return len(inputs)


def _vector(value: object, index: int) -> tuple[float, ...]:
    if not isinstance(value, list) or not value:
        raise _shape_error(f"embedding {index} is not a non-empty array")
    numbers: list[float] = []
    for component in value:
        if isinstance(component, bool) or not isinstance(component, (int, float)):
            raise _shape_error(f"embedding {index} carries a non-numeric component")
        number = float(component)
        if not math.isfinite(number):
            raise _shape_error(f"embedding {index} carries a non-finite component")
        numbers.append(number)
    return tuple(numbers)


def _shape_error(detail: str) -> GatewayError:
    return GatewayError(GatewayErrorCode.PROVIDER_UNAVAILABLE, f"{detail} (rule {RULE_RESPONSE_SHAPE})")


def _error_from_payload(payload: object) -> GatewayError:
    """Classify a provider error object without echoing its text (it may quote the input)."""
    code = payload.get("code") if isinstance(payload, Mapping) else None
    if code in {"rate_limit_exceeded", "insufficient_quota"}:
        return GatewayError(
            GatewayErrorCode.PROVIDER_RATE_LIMITED,
            f"provider rate limited the embedding call (rule {RULE_RESPONSE_SHAPE})",
            retryable=True,
        )
    if code in {"invalid_request_error", "invalid_api_key", "model_not_found"}:
        return GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"provider refused the embedding request (rule {RULE_RESPONSE_SHAPE})",
        )
    return GatewayError(
        GatewayErrorCode.PROVIDER_UNAVAILABLE,
        f"provider reported an embedding failure (rule {RULE_RESPONSE_SHAPE})",
    )


def check_width(vectors: Sequence[Sequence[float]], *, expected: int, model_catalog_id: str) -> int:
    """Every vector must carry the route's declared width; the width is returned for the caller."""
    for position, vector in enumerate(vectors):
        if len(vector) != expected:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"route {model_catalog_id} produced a {len(vector)}-wide vector at {position}, "
                f"the catalog declares {expected} (rule {RULE_RESPONSE_WIDTH})",
            )
    return expected
