"""ScopeContext validation and deadline enforcement for every intelligence RPC.

DOSSIER.md §4.2 requires every Rust → Python call to carry tenant/workspace scope, a
correlation id, a deadline and a capability projection id. This module is the single gate
that enforces it before any handler logic runs: a call without a valid `ScopeContext` is
aborted with a canonical typed error and is never processed.

Scope resolution order:

1. the request message's `scope` field, when the message declares one of type `ScopeContext`
   (`ContextBuildRequest`, `MemoryCandidate`, `EmbedRequest`, `TrustClassifyRequest`,
   `EvaluationRequest`);
2. otherwise the `quansio-scope-bin` invocation metadata carrying a serialized `ScopeContext`.

`SearchProgram` cannot carry a `ScopeContext` today (its `scope` field is the typed retrieval
IR enum), so `Search` takes its scope from metadata. Extending that proto is additive work for
the task that implements the method; the boundary fails closed in the meantime.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass

import grpc
from quansio.v1.intelligence import service_pb2

from intelligence.server.errors import ErrorCode, ErrorDetail, ErrorEnvelope

SCOPE_METADATA_KEY = "quansio-scope-bin"

REQUIRED_SCOPE_FIELDS: tuple[str, ...] = (
    "tenant_id",
    "workspace_id",
    "correlation_id",
    "capability_projection_id",
)


@dataclass(frozen=True, slots=True)
class CallScope:
    """Validated scope of one call."""

    tenant_id: str
    workspace_id: str
    correlation_id: str
    capability_projection_id: str
    deadline_ms: int


class ScopeViolationError(Exception):
    """Typed scope or deadline failure; carries the envelope the caller aborts with."""

    def __init__(self, envelope: ErrorEnvelope, status: grpc.StatusCode) -> None:
        super().__init__(envelope.message)
        self.envelope = envelope
        self.status = status


def _message_scope(request: object) -> service_pb2.ScopeContext | None:
    """The request's own `scope` field, if it is a populated `ScopeContext`."""
    descriptor = getattr(request, "DESCRIPTOR", None)
    fields_by_name = getattr(descriptor, "fields_by_name", None)
    getter = getattr(fields_by_name, "get", None)
    field = getter("scope") if callable(getter) else None
    message_type = getattr(field, "message_type", None)
    if message_type is None or message_type.full_name != service_pb2.ScopeContext.DESCRIPTOR.full_name:
        return None
    has_field = getattr(request, "HasField", None)
    if not callable(has_field) or not has_field("scope"):
        return None
    value = getattr(request, "scope", None)
    return value if isinstance(value, service_pb2.ScopeContext) else None


def _metadata_scope(
    invocation_metadata: Sequence[tuple[str, str | bytes]],
) -> service_pb2.ScopeContext | None:
    for key, value in invocation_metadata:
        if key != SCOPE_METADATA_KEY:
            continue
        if not isinstance(value, (bytes, bytearray)):
            raise ScopeViolationError(
                ErrorEnvelope(
                    code=ErrorCode.VALIDATION_SCHEMA,
                    message=f"{SCOPE_METADATA_KEY} must carry serialized ScopeContext bytes",
                    details=(ErrorDetail("field", "scope"),),
                ),
                grpc.StatusCode.INVALID_ARGUMENT,
            )
        message = service_pb2.ScopeContext()
        try:
            message.ParseFromString(bytes(value))
        except Exception as error:  # protobuf raises DecodeError
            raise ScopeViolationError(
                ErrorEnvelope(
                    code=ErrorCode.VALIDATION_SCHEMA,
                    message=f"{SCOPE_METADATA_KEY} is not a valid ScopeContext",
                    details=(ErrorDetail("field", "scope"),),
                ),
                grpc.StatusCode.INVALID_ARGUMENT,
            ) from error
        return message
    return None


def _violation(
    message: str,
    *,
    correlation_id: str = "",
    details: tuple[ErrorDetail, ...] = (),
) -> ScopeViolationError:
    return ScopeViolationError(
        ErrorEnvelope(
            code=ErrorCode.VALIDATION_SCHEMA,
            message=message,
            correlation_id=correlation_id,
            retryable=False,
            details=details,
        ),
        grpc.StatusCode.INVALID_ARGUMENT,
    )


def validate_call_scope(
    request: object,
    invocation_metadata: Sequence[tuple[str, str | bytes]],
    context: grpc.ServicerContext,
    *,
    now_ms: int,
) -> CallScope:
    """Validate the call's `ScopeContext`; raise `ScopeViolationError` when it is unusable."""
    scope = _message_scope(request)
    if scope is None:
        scope = _metadata_scope(invocation_metadata)
    if scope is None:
        raise _violation(
            "ScopeContext is required: tenant_id, workspace_id, correlation_id, deadline_ms "
            "and capability_projection_id (DOSSIER.md §4.2)",
            details=(ErrorDetail("field", "scope"),),
        )

    missing = tuple(name for name in REQUIRED_SCOPE_FIELDS if not getattr(scope, name, "").strip())
    if missing:
        raise _violation(
            "ScopeContext field(s) missing or empty: " + ", ".join(missing),
            correlation_id=scope.correlation_id,
            details=tuple(ErrorDetail("field", name) for name in missing),
        )

    if scope.deadline_ms <= 0:
        raise _violation(
            "ScopeContext.deadline_ms is required",
            correlation_id=scope.correlation_id,
            details=(ErrorDetail("field", "deadline_ms"),),
        )
    if scope.deadline_ms <= now_ms:
        raise ScopeViolationError(
            ErrorEnvelope(
                code=ErrorCode.TOOL_TIMEOUT,
                message="request deadline expired before processing",
                correlation_id=scope.correlation_id,
                details=(
                    ErrorDetail("deadline_ms", str(scope.deadline_ms)),
                    ErrorDetail("now_ms", str(now_ms)),
                ),
            ),
            grpc.StatusCode.DEADLINE_EXCEEDED,
        )

    remaining = context.time_remaining()
    if remaining is not None and remaining <= 0:
        raise ScopeViolationError(
            ErrorEnvelope(
                code=ErrorCode.TOOL_TIMEOUT,
                message="gRPC call deadline expired before processing",
                correlation_id=scope.correlation_id,
            ),
            grpc.StatusCode.DEADLINE_EXCEEDED,
        )

    return CallScope(
        tenant_id=scope.tenant_id,
        workspace_id=scope.workspace_id,
        correlation_id=scope.correlation_id,
        capability_projection_id=scope.capability_projection_id,
        deadline_ms=scope.deadline_ms,
    )
