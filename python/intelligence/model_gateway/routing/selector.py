"""Deterministic route selection (INT-003, DOMAIN.md §11.1).

The selector is pure: the same (catalog, request, policy) always produces the same route, the
same rule id and the same route id, and it performs no I/O of any kind — so the normal
primary-model path makes **zero** calls to any model before the selected one (acceptance 1).

Resolution order, each step recording its rule id:

1. `policy.user_choice` — `ModelCallRequest.route_hint` names an explicit model. An unknown or
   ineligible hint fails closed with `ROUTE_UNAVAILABLE`; a user choice is never silently
   substituted, because that would hide a capability or DLP problem.
2. `policy.request_class` — the class the caller declared, else derived from the request
   (tools present → `tool_heavy`; `max_output_tokens == 0` and no messages → `embedding`).
3. Eligibility: `policy.capability` (the class's required capabilities), `policy.dlp_eligibility`
   (a model whose provider forbids the profile cannot be selected at all) and
   `policy.context_window` (the requested output must fit).
4. Preferences: the class's `preferred` ids first, then `policy.cost_preference` /
   `policy.quality_preference` over the remaining eligible models, with the catalog id as the
   tie-break so the order never depends on mapping iteration order.
5. `catalog.routing.primary` / `catalog.routing.fallback` supply the tail, bounded by the class's
   `max_fallbacks`.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from datetime import UTC, datetime

from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway.catalog import ModelCatalog, ModelSpec
from intelligence.model_gateway.errors import RouteUnavailableError
from intelligence.model_gateway.ids import new_ulid
from intelligence.model_gateway.routing.policy import (
    RULE_CAPABILITY,
    RULE_CATALOG_FALLBACK,
    RULE_CATALOG_PRIMARY,
    RULE_COST_PREFERENCE,
    RULE_QUALITY_PREFERENCE,
    RULE_REQUEST_CLASS,
    RULE_ROUTE_HINT,
    RULE_USER_CHOICE,
    ClassPreference,
    Preference,
    RequestClass,
    RoutePolicy,
)
from intelligence.model_gateway.selection import RouteDecision

SCHEMA_VERSION = "v1"

#: The proto members for each request class; the enum is the wire authority, this maps the
#: policy's spelling onto it.
REQUEST_CLASS_PROTO = {
    RequestClass.CHAT: intelligence_pb2.ModelRoute.REQUEST_CLASS_CHAT,
    RequestClass.PLANNING: intelligence_pb2.ModelRoute.REQUEST_CLASS_PLANNING,
    RequestClass.TOOL_HEAVY: intelligence_pb2.ModelRoute.REQUEST_CLASS_TOOL_HEAVY,
    RequestClass.SYNTHESIS: intelligence_pb2.ModelRoute.REQUEST_CLASS_SYNTHESIS,
    RequestClass.VERIFICATION: intelligence_pb2.ModelRoute.REQUEST_CLASS_VERIFICATION,
    RequestClass.EMBEDDING: intelligence_pb2.ModelRoute.REQUEST_CLASS_EMBEDDING,
    RequestClass.CHEAP_WORKER: intelligence_pb2.ModelRoute.REQUEST_CLASS_CHEAP_WORKER,
}

#: Cost classes ordered cheapest first; an unknown class sorts last so it is never preferred
#: accidentally.
COST_RANK: dict[str, int] = {"low": 0, "medium": 1, "high": 2}


def classify(request: intelligence_pb2.ModelCallRequest) -> RequestClass:
    """Derive the request class from the request's own shape.

    `ModelCallRequest` carries no class field, so a caller that is planning, verifying or
    synthesising declares its class on the selector ([`PolicyRouteSelector`]); this derivation is
    the fallback for a call that says nothing about itself.
    """
    if request.tools:
        return RequestClass.TOOL_HEAVY
    if request.max_output_tokens == 0 and not request.messages:
        return RequestClass.EMBEDDING
    return RequestClass.CHAT


class PolicyRouteSelector:
    """The INT-003 selector: policy-driven, deterministic, and I/O free."""

    __slots__ = ("_default_class", "_policy")

    def __init__(
        self,
        policy: RoutePolicy | None = None,
        default_class: RequestClass | None = None,
    ) -> None:
        self._policy = policy or RoutePolicy.builtin()
        self._default_class = default_class

    @property
    def policy(self) -> RoutePolicy:
        return self._policy

    @property
    def default_class(self) -> RequestClass | None:
        """The class this selector declares, when it declares one."""
        return self._default_class

    def select(self, catalog: ModelCatalog, request: intelligence_pb2.ModelCallRequest) -> RouteDecision:
        request_class = self._default_class or classify(request)
        preference = self._policy.for_class(request_class)
        required = set(preference.require_capabilities)

        def eligible(model: ModelSpec) -> tuple[bool, str]:
            # Routing decides fitness for the *task*. A model's data-policy clearance is enforced
            # by the DLP guard before transmission, and an output bound that does not fit is a
            # request-shape error raised by `prepare()`, so neither is re-litigated here.
            if not all(model.supports(capability) for capability in required):
                return False, RULE_CAPABILITY
            return True, RULE_REQUEST_CLASS

        hint = request.route_hint.strip()
        if hint:
            # A refusal has no route to carry a rule id, so the rule that refused it is named in
            # the message; every accepted decision records its rule in `chosen_by`.
            if hint not in catalog.models:
                raise RouteUnavailableError(
                    f"route hint {hint!r} is not a catalog model (rule {RULE_USER_CHOICE})"
                )
            model = catalog.models[hint]
            ok, rule = eligible(model)
            if not ok:
                raise RouteUnavailableError(
                    f"route hint {hint!r} is not eligible ({rule}; rule {RULE_USER_CHOICE})"
                )
            # An explicit choice keeps the rule id INT-002 established for it: the vocabulary is
            # the contract a route is explained with, and this decision is unchanged.
            rule_id = RULE_ROUTE_HINT
            ordered = [model]
        else:
            ordered, rule_id = self._order(catalog, preference, eligible)

        if not ordered:
            raise RouteUnavailableError(
                f"no catalog model satisfies request class {request_class} (rule {RULE_REQUEST_CLASS})"
            )
        primary = ordered[0]
        fallbacks = [model.id for model in ordered[1 : 1 + preference.max_fallbacks]]
        remaining = [
            model
            for model in catalog.fallback_order
            if model in catalog.models and model not in {primary.id, *fallbacks}
        ][: max(0, preference.max_fallbacks - len(fallbacks))]
        fallbacks.extend(remaining)
        provider = catalog.provider_for(primary)
        route = intelligence_pb2.ModelRoute(
            schema_version=SCHEMA_VERSION,
            id=f"mr_{new_ulid(seed=f'{request.call_id}|{primary.id}|{rule_id}')}",
            request_class=REQUEST_CLASS_PROTO[request_class],
            provider=provider.name,
            model_id=primary.model_id,
            config_json=json.dumps(
                {
                    "effort": request.effort,
                    "max_tokens": request.max_output_tokens,
                    "model_catalog_id": primary.id,
                    "preference": preference.prefer.value,
                },
                sort_keys=True,
                separators=(",", ":"),
            ),
            dlp_profile=request.dlp_profile,
            fallbacks=fallbacks,
            cost_class=primary.cost_class,
            chosen_by=rule_id,
            selected_at=datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z"),
        )
        return RouteDecision(route=route, model=primary, provider=provider)

    def _order(
        self,
        catalog: ModelCatalog,
        preference: ClassPreference,
        eligible: Callable[[ModelSpec], tuple[bool, str]],
    ) -> tuple[list[ModelSpec], str]:
        """Order the eligible models, and name the rule that decided the primary."""
        eligible_models = [model for model in catalog.models.values() if eligible(model)[0]]
        preferred = [
            catalog.models[model_id]
            for model_id in preference.preferred
            if model_id in catalog.models and eligible(catalog.models[model_id])[0]
        ]
        chosen_rule = RULE_REQUEST_CLASS
        if preference.prefer is Preference.COST:
            ranked = sorted(
                eligible_models,
                key=lambda model: (COST_RANK.get(model.cost_class, 99), model.id),
            )
            if ranked:
                chosen_rule = RULE_COST_PREFERENCE
        else:
            ranked = sorted(
                eligible_models,
                key=lambda model: (-len(model.capabilities), -model.context_window, model.id),
            )
            if ranked:
                chosen_rule = RULE_QUALITY_PREFERENCE
        primary = catalog.models.get(catalog.routing_primary)
        if primary is not None and eligible(primary)[0]:
            # The catalog's primary is the deployment's stated preference: it leads unless the
            # class names its own preferred ids.
            ordered = (
                [primary]
                if not preferred
                else preferred + [model for model in [primary] if model.id not in {m.id for m in preferred}]
            )
            ordered.extend(model for model in ranked if model.id not in {m.id for m in ordered})
            chosen_rule = RULE_CATALOG_PRIMARY
            return ordered, chosen_rule
        ordered = preferred + [model for model in ranked if model.id not in {m.id for m in preferred}]
        if not ordered:
            chosen_rule = RULE_CATALOG_FALLBACK
        return ordered, chosen_rule


def bounded_failover[T](
    decision: RouteDecision,
    catalog: ModelCatalog,
    attempt: Callable[[ModelSpec], T],
    *,
    is_retryable: Callable[[Exception], bool],
    max_attempts: int,
) -> tuple[T, ModelSpec]:
    """Try the primary, then the route's fallbacks, bounded and identity-preserving.

    The attempt callable is handed one model at a time. Every attempt reuses the caller's own
    request object — the driver never builds a request — so the request identity (`call_id`) is
    preserved across failover, which is what lets the runtime correlate a retry with the run that
    asked for it (INT-003 build item 3).

    Raises the last error when every attempt fails or the bound is reached.
    """
    if max_attempts < 1:
        raise ValueError("max_attempts must be at least 1")
    models = [decision.model]
    for model_id in decision.route.fallbacks:
        model = catalog.model(model_id)
        if model is not None:
            models.append(model)
    last: Exception | None = None
    for index, model in enumerate(models[:max_attempts]):
        try:
            return attempt(model), model
        except Exception as error:
            last = error
            if not is_retryable(error) or index + 1 == min(len(models), max_attempts):
                raise
    raise last if last is not None else RuntimeError("failover exhausted")
