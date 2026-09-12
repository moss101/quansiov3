"""Quansio intelligence gateway (INT-001): the typed Rust ↔ Python service boundary.

This package serves the generated `quansio.v1.intelligence.IntelligenceGateway` gRPC
contract (DOSSIER.md §4.2). `IntelligenceServer` binds either a Unix domain socket (the
co-located default) or loopback TCP (tests/local tooling); non-loopback binding is refused
until mTLS split deployment exists.

Contract guarantees enforced here:

  * every RPC must carry a `ScopeContext` — tenant, workspace, correlation id, deadline and
    capability projection id — from the request message or, for messages that cannot carry
    one, the `quansio-scope-bin` invocation metadata; a call without a valid scope is aborted
    with a canonical DOMAIN.md §15 error and never processed (`intelligence.server.scope`);
  * failures are typed (`intelligence.server.errors`): the canonical `Error` envelope rides
    in the gRPC status details and the `quansio-error-bin` trailer, so an unimplemented method
    is an UNIMPLEMENTED status naming its owner task, never a success-shaped empty message;
  * deadlines and cancellation are honoured before work starts and before a result is
    returned; shutdown drains in-flight calls;
  * Python only proposes. No method mutates canonical runtime, graph, effect or machine
    state, imports a provider SDK or writes an authoritative table.

Rust is the caller for authority-affecting flows; a Rust-side client seam is owned by
INT-002/RUN-*. Run the service with `python -m intelligence.server`.
"""

from __future__ import annotations

from intelligence.server.app import (
    DEFAULT_TCP_PORT,
    DEFAULT_UDS_PATH,
    IntelligenceServer,
    ServerConfig,
    Transport,
    is_loopback_host,
)
from intelligence.server.errors import (
    TRAILING_METADATA_KEY,
    ErrorCode,
    ErrorDetail,
    ErrorEnvelope,
    decode_error,
)
from intelligence.server.scope import SCOPE_METADATA_KEY, CallScope, ScopeViolationError
from intelligence.server.servicer import (
    IMPLEMENTED_METHODS,
    INTELLIGENCE_TASK_ID,
    IntelligenceGatewayServicer,
)

__all__ = [
    "DEFAULT_TCP_PORT",
    "DEFAULT_UDS_PATH",
    "IMPLEMENTED_METHODS",
    "INTELLIGENCE_TASK_ID",
    "SCOPE_METADATA_KEY",
    "TRAILING_METADATA_KEY",
    "CallScope",
    "ErrorCode",
    "ErrorDetail",
    "ErrorEnvelope",
    "IntelligenceGatewayServicer",
    "IntelligenceServer",
    "ScopeViolationError",
    "ServerConfig",
    "Transport",
    "decode_error",
    "is_loopback_host",
]
