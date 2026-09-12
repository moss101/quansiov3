"""Model gateway: provider adapters, normalized `ModelEvent` streaming, cancellation, timeout,
usage accounting and credential custody. Canonical owner: `python/intelligence/model_gateway`
(INT-002, INT-003).

The only place provider SDKs may be imported; provider credentials never leave it, and the
gateway never executes tools, commits WorkGraph success or mutates external systems. All model
fulfillment goes through `ModelGateway` (D-006), which turns a `ModelCallRequest` into a
normalized `ModelEvent` stream per DOMAIN.md §11.1.

Layout:

* `catalog` — `config/models.yaml`: providers, routing, model ids, capabilities, cost classes;
* `credentials` — secret-handle custody; `SecretValue` redacts itself everywhere;
* `transport` — cancellable streaming HTTP/SSE (`http.client`, no provider SDK);
* `content` — canonical rendered content (text/image/document) and its provider encodings;
* `tooling` — strict tool schemas resolved from an injected read-only schema source;
* `selection` — deterministic route resolution with INT-003's replacement seam;
* `adapters` — Anthropic, OpenAI and OpenAI-compatible adapters;
* `conformance` — deterministic offline stub provider and the shared conformance suite.
"""

from __future__ import annotations

from intelligence.model_gateway.catalog import (
    ModelCatalog,
    ModelSpec,
    ProviderConfig,
    ProviderKind,
)
from intelligence.model_gateway.credentials import (
    CredentialResolver,
    SecretValue,
    env_var_for_handle,
)
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.events import TerminalOutcome, UsageTotals
from intelligence.model_gateway.gateway import (
    CancellationRegistry,
    CancellationToken,
    Fulfillment,
    ModelGateway,
    PreparedCall,
)
from intelligence.model_gateway.selection import (
    CatalogPrimarySelector,
    RouteDecision,
    RouteSelector,
)
from intelligence.model_gateway.tooling import (
    MappingToolSchemaSource,
    ToolDefinition,
    ToolSchemaSource,
)
from intelligence.model_gateway.transport import HttpTransport, StdlibHttpTransport

__all__ = [
    "CancellationRegistry",
    "CancellationToken",
    "CatalogPrimarySelector",
    "CredentialResolver",
    "Fulfillment",
    "GatewayError",
    "GatewayErrorCode",
    "HttpTransport",
    "MappingToolSchemaSource",
    "ModelCatalog",
    "ModelGateway",
    "ModelSpec",
    "PreparedCall",
    "ProviderConfig",
    "ProviderKind",
    "RouteDecision",
    "RouteSelector",
    "SecretValue",
    "StdlibHttpTransport",
    "TerminalOutcome",
    "ToolDefinition",
    "ToolSchemaSource",
    "UsageTotals",
    "env_var_for_handle",
]
