"""Shared adapter contract: request shape, provider wire helpers and normalization guardrails.

Every adapter returns a `PreparedRequest` whose body and headers are the *only* bytes that
reach a provider, and every adapter decodes provider payloads into normalized `ModelEvent`s
through the shared `EventFactory`. Three invariants are enforced here, before any byte leaves
the process, because they are architecture and safety requirements rather than provider
features:

* operator-channel instructions never travel as a fake user message;
* prompt-cache hints mark a leading stable prefix only (DOSSIER.md §7);
* no provider-side conversation compaction/context-editing feature is present in the request
  (DOSSIER.md §7: Quansio owns compaction, INT-008).

A violation raises `VALIDATION_SCHEMA` and the call is not transmitted.
"""

from __future__ import annotations

import json
from collections.abc import Iterable, Iterator, Mapping
from dataclasses import dataclass
from typing import Protocol

from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.content import RenderedContent, parse_content
from intelligence.model_gateway.credentials import SecretValue
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.events import EventFactory, UsageTotals
from intelligence.model_gateway.selection import RouteDecision
from intelligence.model_gateway.tooling import ToolDefinition
from intelligence.model_gateway.transport import HttpRequest

SYSTEM_ROLE = intelligence_pb2.RenderedMessage.ROLE_SYSTEM
OPERATOR_ROLE = intelligence_pb2.RenderedMessage.ROLE_OPERATOR
USER_ROLE = intelligence_pb2.RenderedMessage.ROLE_USER
ASSISTANT_ROLE = intelligence_pb2.RenderedMessage.ROLE_ASSISTANT
TOOL_ROLE = intelligence_pb2.RenderedMessage.ROLE_TOOL

# Provider-side conversation compaction / context-editing features that must never be enabled.
# Both snake_case (request bodies) and hyphenated (beta header values) spellings are guarded.
PROVIDER_FEATURES_DISABLED: tuple[str, ...] = (
    "context_management",
    "context-management",
    "context_editing",
    "context-editing",
    "clear_tool_uses",
    "clear-tool-uses",
    "compact",
    "summarization",
    "truncation",
    "previous_response_id",
    "previous-response-id",
)


@dataclass(frozen=True, slots=True)
class RenderedMessageView:
    """One parsed `RenderedMessage`."""

    role: int
    trust_level: str
    cache_hint: str
    content: RenderedContent


@dataclass(frozen=True, slots=True)
class ProviderCallContext:
    """Everything an adapter may use to build one provider request."""

    call: intelligence_pb2.ModelCallRequest
    route: RouteDecision
    credential: SecretValue
    tools: tuple[ToolDefinition, ...]
    messages: tuple[RenderedMessageView, ...]
    base_url: str


@dataclass(frozen=True, slots=True)
class PreparedRequest:
    """The exact request an adapter will transmit; safe to inspect and to log by `repr`."""

    provider: str
    provider_kind: ProviderKind
    model_catalog_id: str
    url: str
    headers: Mapping[str, str | SecretValue]
    body: bytes
    timeout_seconds: float
    disabled_features: tuple[str, ...]

    def as_http_request(self) -> HttpRequest:
        return HttpRequest(
            method="POST",
            url=self.url,
            headers=self.headers,
            body=self.body,
            timeout_seconds=self.timeout_seconds,
        )

    def body_json(self) -> dict[str, object]:
        decoded = json.loads(self.body)
        if not isinstance(decoded, dict):
            raise GatewayError(GatewayErrorCode.INTERNAL, "adapter produced a non-object body")
        return decoded

    def __repr__(self) -> str:
        return (
            f"PreparedRequest(provider={self.provider!r}, url={self.url!r}, "
            f"headers={sorted(self.headers)}, body_bytes={len(self.body)}, "
            f"timeout_seconds={self.timeout_seconds!r}, "
            f"disabled_features={list(self.disabled_features)})"
        )


@dataclass(frozen=True, slots=True)
class ProviderResult:
    """Terminal provider outcome; the gateway turns it into USAGE/STOP/ERROR/COMPLETED events."""

    stop_reason: int
    usage: UsageTotals
    error_code: str | None = None
    retryable: bool = False


type StreamItem = intelligence_pb2.ModelEvent | ProviderResult


class ProviderAdapter(Protocol):
    """One provider wire family, normalized to the shared `ModelEvent` contract."""

    kind: ProviderKind

    def build_request(self, context: ProviderCallContext) -> PreparedRequest: ...

    def decode(self, factory: EventFactory, lines: Iterator[bytes]) -> Iterator[StreamItem]: ...


def parse_messages(
    call: intelligence_pb2.ModelCallRequest,
) -> tuple[RenderedMessageView, ...]:
    """Parse and validate every `RenderedMessage`; unknown roles fail closed."""
    views: list[RenderedMessageView] = []
    for message in call.messages:
        if message.role not in (SYSTEM_ROLE, OPERATOR_ROLE, USER_ROLE, ASSISTANT_ROLE, TOOL_ROLE):
            raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "a rendered message has no usable role")
        views.append(
            RenderedMessageView(
                role=message.role,
                trust_level=message.trust_level,
                cache_hint=message.cache_hint,
                content=parse_content(message.content_json, is_tool_message=message.role == TOOL_ROLE),
            )
        )
    assert_stable_prefix(views)
    return tuple(views)


def assert_stable_prefix(messages: Iterable[RenderedMessageView]) -> None:
    """Prompt-cache hints must mark a leading run: stable content first (DOSSIER.md §7)."""
    volatile_seen = False
    for message in messages:
        if message.cache_hint:
            if volatile_seen:
                raise GatewayError(
                    GatewayErrorCode.VALIDATION_SCHEMA,
                    "a prompt-cache hint follows non-hinted content; stable prefix must come first",
                )
            continue
        volatile_seen = True


def assert_modality_capabilities(
    messages: Iterable[RenderedMessageView], capabilities: frozenset[str]
) -> None:
    """Multimodal parts must be declared by the catalog model; nothing is silently dropped."""
    for message in messages:
        if message.content.has_image and "vision" not in capabilities:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                "the selected model does not declare the `vision` capability",
            )
        if message.content.has_document and "documents" not in capabilities:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                "the selected model does not declare the `documents` capability",
            )


def assert_operator_channel(messages: Iterable[RenderedMessageView], body: Mapping[str, object]) -> None:
    """Operator/system instructions must never be rendered as a user-role message."""
    operator_texts = {
        message.content.text()
        for message in messages
        if message.role in (SYSTEM_ROLE, OPERATOR_ROLE) and message.content.text()
    }
    if not operator_texts:
        return
    raw_messages = body.get("messages", [])
    if not isinstance(raw_messages, list):
        return
    for entry in raw_messages:
        if not isinstance(entry, dict) or entry.get("role") != "user":
            continue
        text = _message_text(entry.get("content"))
        for operator_text in operator_texts:
            if operator_text in text:
                raise GatewayError(
                    GatewayErrorCode.VALIDATION_SCHEMA,
                    "operator instructions must use the provider operator channel, not a user message",
                )


def assert_no_disabled_features(body: bytes, headers: Mapping[str, str | SecretValue]) -> None:
    """Fail closed if any compaction/context-editing feature is present in the request."""
    try:
        decoded = json.loads(body)
    except json.JSONDecodeError as error:  # pragma: no cover - adapters build JSON bodies
        raise GatewayError(GatewayErrorCode.INTERNAL, "adapter produced a body that is not JSON") from error
    _check_keys(decoded)
    for name, value in headers.items():
        haystack = name if isinstance(value, SecretValue) else f"{name}:{value}"
        lowered = haystack.lower()
        for feature in PROVIDER_FEATURES_DISABLED:
            if feature in lowered:
                raise GatewayError(
                    GatewayErrorCode.VALIDATION_SCHEMA,
                    f"provider feature '{feature}' must not be enabled (Quansio owns compaction)",
                )


def _check_keys(node: object) -> None:
    if isinstance(node, dict):
        for key, value in node.items():
            lowered = str(key).lower()
            for feature in PROVIDER_FEATURES_DISABLED:
                if feature in lowered:
                    raise GatewayError(
                        GatewayErrorCode.VALIDATION_SCHEMA,
                        f"provider feature '{feature}' must not be enabled (Quansio owns compaction)",
                    )
            _check_keys(value)
    elif isinstance(node, list):
        for item in node:
            _check_keys(item)


def _message_text(content: object) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(
            str(block.get("text", ""))
            for block in content
            if isinstance(block, dict) and isinstance(block.get("text"), str)
        )
    return ""


def iter_sse(lines: Iterator[bytes]) -> Iterator[tuple[str, str]]:
    """Server-sent events as (event name, data) pairs; comments and blank lines are framing."""
    event = ""
    data_lines: list[str] = []
    for raw in lines:
        line = raw.decode("utf-8", errors="replace")
        if line == "":
            if data_lines:
                yield event, "\n".join(data_lines)
            event = ""
            data_lines = []
            continue
        if line.startswith(":"):
            continue
        field, _, value = line.partition(":")
        if value.startswith(" "):
            value = value[1:]
        if field == "event":
            event = value
        elif field == "data":
            data_lines.append(value)
    if data_lines:
        yield event, "\n".join(data_lines)


def decode_json(data: str) -> dict[str, object]:
    """Decode one SSE payload object; providers can also send a bare JSON error body."""
    try:
        decoded = json.loads(data)
    except json.JSONDecodeError as error:
        raise GatewayError(
            GatewayErrorCode.PROVIDER_UNAVAILABLE, "provider sent a malformed stream payload"
        ) from error
    if not isinstance(decoded, dict):
        raise GatewayError(GatewayErrorCode.PROVIDER_UNAVAILABLE, "provider sent a non-object stream payload")
    return decoded


def http_error_result(status: int, body: bytes) -> ProviderResult:
    """Map a non-2xx provider response to a typed, retryability-annotated outcome.

    The response body is deliberately ignored beyond classification: it can echo request
    content, and the gateway never logs or surfaces it.
    """
    del body
    if status in (401, 403):
        return ProviderResult(
            stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
            usage=UsageTotals(),
            error_code=str(GatewayErrorCode.PROVIDER_UNAVAILABLE),
            retryable=False,
        )
    if status == 429:
        return ProviderResult(
            stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
            usage=UsageTotals(),
            error_code=str(GatewayErrorCode.PROVIDER_RATE_LIMITED),
            retryable=True,
        )
    if status in (408, 409, 425) or status >= 500:
        return ProviderResult(
            stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
            usage=UsageTotals(),
            error_code=str(GatewayErrorCode.PROVIDER_UNAVAILABLE),
            retryable=True,
        )
    return ProviderResult(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
        usage=UsageTotals(),
        error_code=str(GatewayErrorCode.VALIDATION_SCHEMA),
        retryable=False,
    )
