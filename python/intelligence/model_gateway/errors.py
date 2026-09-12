"""Typed model-gateway failures (DOMAIN.md §15).

Codes are the canonical taxonomy values for the Model/Execution families; the gateway adds no
private codes. Every failure carries `retryable` so the runtime can decide whether a bounded
retry is safe (DOMAIN.md §15, DOSSIER.md §7). Messages name the failing component and never
carry prompt content, credentials or provider response bodies.
"""

from __future__ import annotations

from enum import StrEnum


class GatewayErrorCode(StrEnum):
    """DOMAIN.md §15 codes the model gateway can produce."""

    VALIDATION_SCHEMA = "VALIDATION_SCHEMA"
    VALIDATION_BOUNDS = "VALIDATION_BOUNDS"
    ROUTE_UNAVAILABLE = "ROUTE_UNAVAILABLE"
    DLP_DENIED = "DLP_DENIED"
    PROVIDER_UNAVAILABLE = "PROVIDER_UNAVAILABLE"
    PROVIDER_RATE_LIMITED = "PROVIDER_RATE_LIMITED"
    PROVIDER_REFUSAL = "PROVIDER_REFUSAL"
    TOOL_TIMEOUT = "TOOL_TIMEOUT"
    INTERNAL = "INTERNAL"


class GatewayError(Exception):
    """A typed gateway failure that is safe to surface to the caller.

    `message` is operator-facing metadata only: it may name the provider, model catalog id and
    HTTP status, never a credential, a prompt, or a response body.
    """

    __slots__ = ("code", "message", "retryable")

    def __init__(self, code: GatewayErrorCode, message: str, *, retryable: bool = False) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message
        self.retryable = retryable


class RouteUnavailableError(GatewayError):
    """No route could be resolved deterministically for the request."""

    __slots__ = ()

    def __init__(self, message: str) -> None:
        super().__init__(GatewayErrorCode.ROUTE_UNAVAILABLE, message, retryable=False)


class ProviderConfigError(GatewayError):
    """The catalog entry for a provider or model is unusable."""

    __slots__ = ()

    def __init__(self, message: str) -> None:
        super().__init__(GatewayErrorCode.PROVIDER_UNAVAILABLE, message, retryable=False)
