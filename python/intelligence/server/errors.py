"""Typed gRPC error envelope for the intelligence boundary (DOMAIN.md §15).

Product error codes are the canonical DOMAIN.md §15 taxonomy values; the boundary adds no
private codes. Because the intelligence service contract returns messages only (no error
message in `service.proto`), the envelope travels as the gRPC status details text and as the
`quansio-error-bin` trailing metadata (canonical `Error` shape serialized as JSON), so a
caller still has a machine-readable failure when it inspects only one of them.

An intelligence RPC never answers a rejected or unfinished call with a success-shaped empty
message: every failure path goes through `abort_with`.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from enum import StrEnum
from typing import NoReturn

import grpc

TRAILING_METADATA_KEY = "quansio-error-bin"


class ErrorCode(StrEnum):
    """DOMAIN.md §15 codes emitted by the intelligence boundary."""

    VALIDATION_SCHEMA = "VALIDATION_SCHEMA"
    VALIDATION_BOUNDS = "VALIDATION_BOUNDS"
    SCOPE_FORBIDDEN = "SCOPE_FORBIDDEN"
    TOOL_TIMEOUT = "TOOL_TIMEOUT"
    INTERNAL = "INTERNAL"
    # Model family (DOMAIN.md §15): emitted by the model gateway (INT-002) and surfaced here.
    ROUTE_UNAVAILABLE = "ROUTE_UNAVAILABLE"
    DLP_DENIED = "DLP_DENIED"
    PROVIDER_UNAVAILABLE = "PROVIDER_UNAVAILABLE"
    PROVIDER_RATE_LIMITED = "PROVIDER_RATE_LIMITED"
    PROVIDER_REFUSAL = "PROVIDER_REFUSAL"


_GRPC_STATUS_BY_CODE: dict[ErrorCode, grpc.StatusCode] = {
    ErrorCode.VALIDATION_SCHEMA: grpc.StatusCode.INVALID_ARGUMENT,
    ErrorCode.VALIDATION_BOUNDS: grpc.StatusCode.INVALID_ARGUMENT,
    ErrorCode.SCOPE_FORBIDDEN: grpc.StatusCode.PERMISSION_DENIED,
    # The taxonomy has no DEADLINE_EXCEEDED code; a request whose deadline expired before
    # processing (or during a call the runtime bounded) is the Execution family's timeout.
    ErrorCode.TOOL_TIMEOUT: grpc.StatusCode.DEADLINE_EXCEEDED,
    ErrorCode.INTERNAL: grpc.StatusCode.INTERNAL,
    ErrorCode.ROUTE_UNAVAILABLE: grpc.StatusCode.FAILED_PRECONDITION,
    ErrorCode.DLP_DENIED: grpc.StatusCode.PERMISSION_DENIED,
    ErrorCode.PROVIDER_UNAVAILABLE: grpc.StatusCode.UNAVAILABLE,
    ErrorCode.PROVIDER_RATE_LIMITED: grpc.StatusCode.UNAVAILABLE,
    ErrorCode.PROVIDER_REFUSAL: grpc.StatusCode.FAILED_PRECONDITION,
}


@dataclass(frozen=True, slots=True)
class ErrorDetail:
    """One `{key, value}` entry of the canonical `Error.details` list."""

    key: str
    value: str


@dataclass(frozen=True, slots=True)
class ErrorEnvelope:
    """Canonical `Error` (`code, message, correlation_id, retryable, details?`)."""

    code: ErrorCode
    message: str
    correlation_id: str = ""
    retryable: bool = False
    details: tuple[ErrorDetail, ...] = ()

    def as_dict(self) -> dict[str, object]:
        return {
            "code": str(self.code),
            "message": self.message,
            "correlation_id": self.correlation_id,
            "retryable": self.retryable,
            "details": [{"key": detail.key, "value": detail.value} for detail in self.details],
        }

    def to_json(self) -> str:
        return json.dumps(self.as_dict(), sort_keys=True, separators=(",", ":"))


def status_for(code: ErrorCode) -> grpc.StatusCode:
    """gRPC status mapped from a canonical error code."""
    return _GRPC_STATUS_BY_CODE[code]


def abort_with(
    context: grpc.ServicerContext,
    envelope: ErrorEnvelope,
    status: grpc.StatusCode | None = None,
) -> NoReturn:
    """Terminate the RPC with a typed error. Never returns."""
    payload = envelope.to_json()
    context.set_trailing_metadata(((TRAILING_METADATA_KEY, payload.encode("utf-8")),))
    context.abort(status if status is not None else status_for(envelope.code), payload)
    # grpc.ServicerContext.abort always raises; this keeps the NoReturn contract honest.
    raise RuntimeError("gRPC abort returned control to the caller")


def _envelope_from_dict(data: dict[str, object]) -> ErrorEnvelope:
    raw_code = data.get("code")
    try:
        code = ErrorCode(str(raw_code))
    except ValueError:
        code = ErrorCode.INTERNAL
    raw_details = data.get("details")
    details: tuple[ErrorDetail, ...] = ()
    if isinstance(raw_details, list):
        details = tuple(
            ErrorDetail(str(entry.get("key", "")), str(entry.get("value", "")))
            for entry in raw_details
            if isinstance(entry, dict)
        )
    return ErrorEnvelope(
        code=code,
        message=str(data.get("message", "")),
        correlation_id=str(data.get("correlation_id", "")),
        retryable=bool(data.get("retryable", False)),
        details=details,
    )


def decode_error(error: grpc.RpcError) -> ErrorEnvelope:
    """Decode the typed envelope from a failed call, falling back to the gRPC status.

    Used by the boundary tests and by any in-process Python caller; the Rust caller decodes
    the same payload from the `quansio-error-bin` trailer.
    """
    for key, value in error.trailing_metadata() or ():
        if key != TRAILING_METADATA_KEY or not isinstance(value, (bytes, bytearray)):
            continue
        try:
            decoded = json.loads(bytes(value).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            break
        if isinstance(decoded, dict):
            return _envelope_from_dict(decoded)
    return ErrorEnvelope(code=ErrorCode.INTERNAL, message=error.details() or "")
