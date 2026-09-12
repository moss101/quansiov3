"""Deterministic model selection and bounded failover (INT-003, DOMAIN.md §11.1).

Canonical owner: `python/intelligence/model_gateway/routing`. Selection is policy over the
catalog: request classes, capability demand, DLP eligibility, context fit, cost/quality
preference and explicit user choice, each recording the rule id that decided it. The selector is
pure and I/O free, so a primary-model path performs zero auxiliary model calls before the model
the caller actually needs.
"""

from __future__ import annotations

from intelligence.model_gateway.routing.policy import (
    REQUEST_CLASSES,
    ClassPreference,
    Preference,
    RequestClass,
    RoutePolicy,
)
from intelligence.model_gateway.routing.selector import (
    COST_RANK,
    REQUEST_CLASS_PROTO,
    PolicyRouteSelector,
    bounded_failover,
    classify,
)
from intelligence.model_gateway.selection import RouteDecision

__all__ = [
    "COST_RANK",
    "REQUEST_CLASSES",
    "REQUEST_CLASS_PROTO",
    "ClassPreference",
    "PolicyRouteSelector",
    "Preference",
    "RequestClass",
    "RouteDecision",
    "RoutePolicy",
    "bounded_failover",
    "classify",
]
