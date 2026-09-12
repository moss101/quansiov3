"""Deterministic route resolution — the minimal INT-002 rule, with INT-003's seam.

INT-003 owns model selection policy: request classes, capability demand, DLP eligibility,
cost/latency preference, failover and rule ids. INT-002 only needs one obvious, deterministic
rule so `FulfillModel` can be served end to end, and it must perform no model call before the
selected primary model (DOSSIER.md §7). `RouteSelector` is that seam: INT-003 replaces the
selector implementation without touching the gateway or the adapters.

Rules (in order):

1. `catalog.route_hint` — `ModelCallRequest.route_hint` names an explicit user choice; an
   unknown hint fails closed with `ROUTE_UNAVAILABLE` rather than silently falling back.
2. `catalog.routing.primary` — the catalog's `routing.primary` model.

`chosen_by` always carries the rule id, and `RouteDecision.deterministic_key` is a pure
function of (rule id, model id) so a test can prove selection determinism without a clock.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Protocol

from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway.catalog import ModelCatalog, ModelSpec, ProviderConfig
from intelligence.model_gateway.ids import new_ulid

RULE_ROUTE_HINT = "catalog.route_hint"
RULE_PRIMARY = "catalog.routing.primary"

SCHEMA_VERSION = "v1"


@dataclass(frozen=True, slots=True)
class RouteDecision:
    """A resolved `ModelRoute` plus the catalog entries it was built from."""

    route: intelligence_pb2.ModelRoute
    model: ModelSpec
    provider: ProviderConfig

    @property
    def route_id(self) -> str:
        return self.route.id

    @property
    def deterministic_key(self) -> str:
        return f"{self.route.chosen_by}|{self.model.id}"


class RouteSelector(Protocol):
    """Deterministic selection policy; INT-003 owns the production implementation."""

    def select(self, catalog: ModelCatalog, request: intelligence_pb2.ModelCallRequest) -> RouteDecision: ...


class CatalogPrimarySelector:
    """The INT-002 rule: explicit hint, else `routing.primary`. No model call, no I/O."""

    __slots__ = ()

    def select(self, catalog: ModelCatalog, request: intelligence_pb2.ModelCallRequest) -> RouteDecision:
        hint = request.route_hint.strip()
        if hint:
            model = catalog.model(hint)
            rule = RULE_ROUTE_HINT
        else:
            model = catalog.primary()
            rule = RULE_PRIMARY
        provider = catalog.provider_for(model)
        route = intelligence_pb2.ModelRoute(
            schema_version=SCHEMA_VERSION,
            id=f"mr_{new_ulid(seed=f'{request.call_id}|{model.id}|{rule}')}",
            request_class=intelligence_pb2.ModelRoute.REQUEST_CLASS_CHAT,
            provider=provider.name,
            model_id=model.model_id,
            config_json=json.dumps(
                {
                    "effort": request.effort,
                    "max_tokens": request.max_output_tokens,
                    "model_catalog_id": model.id,
                },
                sort_keys=True,
                separators=(",", ":"),
            ),
            dlp_profile=request.dlp_profile,
            fallbacks=list(catalog.fallback_order),
            cost_class=model.cost_class,
            chosen_by=rule,
            selected_at=datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z"),
        )
        return RouteDecision(route=route, model=model, provider=provider)
