"""Request classes and the routing policy they select by (DOMAIN.md §11.1, INT-003).

A policy is data, not code paths: each request class names the capabilities it demands, the
catalog models it prefers, and whether it prefers the cheapest or the strongest eligible model.
The catalog supplies availability and cost; the policy supplies preference. Nothing here calls a
model — the whole point of the class is that a primary-model path needs **zero** auxiliary LLM
calls before the selected model (acceptance 1).

Rule ids are recorded on every decision, so a route can always be explained after the fact.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from enum import StrEnum

#: The request classes DOMAIN.md §11.1 names.
REQUEST_CLASSES: frozenset[str] = frozenset(
    {
        "chat",
        "planning",
        "tool_heavy",
        "synthesis",
        "verification",
        "embedding",
        "cheap_worker",
    }
)

# Rule ids recorded in `ModelRoute.chosen_by`.
RULE_USER_CHOICE = "policy.user_choice"
RULE_REQUEST_CLASS = "policy.request_class"
RULE_CAPABILITY = "policy.capability"
RULE_DLP_ELIGIBILITY = "policy.dlp_eligibility"
RULE_CONTEXT_WINDOW = "policy.context_window"
RULE_COST_PREFERENCE = "policy.cost_preference"
RULE_QUALITY_PREFERENCE = "policy.quality_preference"
RULE_CATALOG_PRIMARY = "catalog.routing.primary"
RULE_CATALOG_FALLBACK = "catalog.routing.fallback"


class RequestClass(StrEnum):
    """What the caller is asking the model to do."""

    CHAT = "chat"
    PLANNING = "planning"
    TOOL_HEAVY = "tool_heavy"
    SYNTHESIS = "synthesis"
    VERIFICATION = "verification"
    EMBEDDING = "embedding"
    CHEAP_WORKER = "cheap_worker"

    @classmethod
    def parse(cls, value: str) -> RequestClass:
        """Parse a request class, refusing an unknown one rather than guessing."""
        try:
            return cls(value.strip().lower())
        except ValueError as error:  # pragma: no cover - message only
            raise ValueError(
                f"{value!r} is not a request class; expected one of {sorted(REQUEST_CLASSES)}"
            ) from error


class Preference(StrEnum):
    """Which eligible model a class prefers."""

    COST = "cost"
    QUALITY = "quality"


@dataclass(frozen=True, slots=True)
class ClassPreference:
    """How one request class chooses among eligible models."""

    request_class: RequestClass
    #: Capabilities every candidate must have.
    require_capabilities: frozenset[str] = frozenset()
    #: Catalog model ids this class tries first, in order.
    preferred: tuple[str, ...] = ()
    #: Cost or quality preference among the remaining eligible candidates.
    prefer: Preference = Preference.COST
    #: How many alternatives the route may carry for failover.
    max_fallbacks: int = 2


@dataclass(frozen=True, slots=True)
class RoutePolicy:
    """The routing policy: one preference per request class, plus a fallback rule."""

    by_class: Mapping[RequestClass, ClassPreference] = field(default_factory=dict)

    def for_class(self, request_class: RequestClass) -> ClassPreference:
        """The preference for a class, defaulting to a plain cost-preferring chat."""
        return self.by_class.get(
            request_class,
            ClassPreference(request_class=RequestClass.CHAT),
        )

    @classmethod
    def builtin(cls) -> RoutePolicy:
        """The shipped policy: capability demands and cost/quality preferences per class.

        Model ids are deliberately absent — the catalog decides which models exist (D-018), and a
        model id in source would be a second authority for it.
        """
        return cls(
            by_class={
                RequestClass.CHAT: ClassPreference(request_class=RequestClass.CHAT),
                RequestClass.PLANNING: ClassPreference(
                    request_class=RequestClass.PLANNING, prefer=Preference.QUALITY
                ),
                RequestClass.TOOL_HEAVY: ClassPreference(
                    request_class=RequestClass.TOOL_HEAVY,
                    require_capabilities=frozenset({"tools"}),
                    prefer=Preference.QUALITY,
                    max_fallbacks=3,
                ),
                RequestClass.SYNTHESIS: ClassPreference(
                    request_class=RequestClass.SYNTHESIS, prefer=Preference.QUALITY
                ),
                RequestClass.VERIFICATION: ClassPreference(
                    request_class=RequestClass.VERIFICATION, prefer=Preference.QUALITY
                ),
                RequestClass.EMBEDDING: ClassPreference(
                    request_class=RequestClass.EMBEDDING,
                    require_capabilities=frozenset({"embeddings"}),
                    prefer=Preference.COST,
                ),
                RequestClass.CHEAP_WORKER: ClassPreference(
                    request_class=RequestClass.CHEAP_WORKER, prefer=Preference.COST
                ),
            }
        )
