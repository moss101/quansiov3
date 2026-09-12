"""Provider adapters: the only place a provider wire format is known.

Each adapter turns a `ModelCallRequest` into a `PreparedRequest` and provider payloads into
normalized `ModelEvent`s. Adapters are selected by the provider `kind` declared in
`config/models.yaml`; no model identifier appears here.
"""

from __future__ import annotations

from intelligence.model_gateway.adapters.anthropic import AnthropicAdapter
from intelligence.model_gateway.adapters.base import (
    PROVIDER_FEATURES_DISABLED,
    PreparedRequest,
    ProviderAdapter,
    ProviderCallContext,
    ProviderResult,
    RenderedMessageView,
    StreamItem,
    http_error_result,
    iter_sse,
    parse_messages,
)
from intelligence.model_gateway.adapters.openai import OpenAIAdapter
from intelligence.model_gateway.adapters.openai_compatible import OpenAICompatibleAdapter
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

ADAPTER_CLASSES: tuple[type[ProviderAdapter], ...] = (
    AnthropicAdapter,
    OpenAIAdapter,
    OpenAICompatibleAdapter,
)


def adapter_for_kind(kind: ProviderKind) -> ProviderAdapter:
    """The single adapter registered for a provider kind; an unknown kind fails closed."""
    for adapter_class in ADAPTER_CLASSES:
        if adapter_class.kind == kind:
            return adapter_class()
    raise GatewayError(
        GatewayErrorCode.PROVIDER_UNAVAILABLE, f"no adapter is registered for provider kind {kind}"
    )


__all__ = [
    "ADAPTER_CLASSES",
    "PROVIDER_FEATURES_DISABLED",
    "AnthropicAdapter",
    "OpenAIAdapter",
    "OpenAICompatibleAdapter",
    "PreparedRequest",
    "ProviderAdapter",
    "ProviderCallContext",
    "ProviderResult",
    "RenderedMessageView",
    "StreamItem",
    "adapter_for_kind",
    "http_error_result",
    "iter_sse",
    "parse_messages",
]
