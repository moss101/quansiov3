"""Data-classification and redaction before any provider call (INT-003, DOMAIN.md §12, §11.1).

The guard runs *before* transmission: a data class the provider or the model is not cleared for
fails with `DLP_DENIED` and nothing is sent (acceptance 2). Redactions are applied to the
request that is actually transmitted, and every decision names its rule id.

Two levels are checked, because they fail for different reasons:

* the **model** (`ModelSpec.dlp_eligible`) — this deployment's model-level clearance, already part
  of route eligibility; and
* the **provider** (`DlpPolicy.allowed_by_provider`) — which data classes are contractually
  allowed to leave for that provider at all.

Redaction is deliberately conservative: a rule replaces a match with a typed placeholder, and the
guard reports which rules fired so the decision is auditable from the route and the usage trail.
"""

from __future__ import annotations

import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from enum import StrEnum

from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway.catalog import ModelSpec, ProviderConfig
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

RULE_DATA_CLASS = "dlp.data_class"
RULE_MODEL_CLEARANCE = "dlp.model_eligibility"
RULE_PROVIDER_CLEARANCE = "dlp.provider_data_class"
RULE_REDACTION = "dlp.redaction"

#: The data classes DOMAIN.md §12 names, most restrictive last.
DATA_CLASSES: tuple[str, ...] = ("public", "internal", "confidential", "restricted")


class DataClass(StrEnum):
    """How sensitive the content of a request is."""

    PUBLIC = "public"
    INTERNAL = "internal"
    CONFIDENTIAL = "confidential"
    RESTRICTED = "restricted"

    @classmethod
    def parse(cls, value: str) -> DataClass:
        """Parse a data class, refusing anything else."""
        try:
            return cls(value.strip().lower())
        except ValueError as error:  # pragma: no cover - message only
            raise ValueError(f"{value!r} is not a data class") from error

    @property
    def rank(self) -> int:
        """Position in the ladder, most permissive first."""
        return DATA_CLASSES.index(self.value)


@dataclass(frozen=True, slots=True)
class Redaction:
    """One redaction rule: a pattern replaced by a typed placeholder."""

    rule_id: str
    pattern: re.Pattern[str]
    placeholder: str


@dataclass(frozen=True, slots=True)
class DlpPolicy:
    """Which providers may receive which data classes, and what must be redacted first."""

    #: Allowed data classes per provider name. A provider absent from the map allows nothing.
    allowed_by_provider: Mapping[str, frozenset[DataClass]] = field(default_factory=dict)
    #: Data class per `dlp_profile` name; an unknown profile takes `default_data_class`.
    data_class_by_profile: Mapping[str, DataClass] = field(default_factory=dict)
    #: Data class for a request that names no profile.
    default_data_class: DataClass = DataClass.INTERNAL
    #: Redaction rules applied to every transmitted message.
    redactions: tuple[Redaction, ...] = ()

    def data_class_for(self, profile: str) -> DataClass:
        """The data class a request declares, by profile."""
        if not profile:
            return self.default_data_class
        return self.data_class_by_profile.get(profile, self.default_data_class)

    @classmethod
    def from_catalog(cls, catalog: object, *, allowed: frozenset[DataClass] | None = None) -> DlpPolicy:
        """The default policy for a catalog: its configured providers, cleared for `allowed`.

        The catalog is the deployment's statement of which providers exist, so they are cleared for
        public and internal content by default; confidential and restricted content still needs an
        explicit policy, so the default cannot leak anything sensitive by omission.
        """
        clearance = allowed if allowed is not None else frozenset({DataClass.PUBLIC, DataClass.INTERNAL})
        providers = getattr(catalog, "providers", {})
        return cls(
            allowed_by_provider={config.name: clearance for config in providers.values()},
            redactions=cls.builtin().redactions,
        )

    @classmethod
    def builtin(cls) -> DlpPolicy:
        """The shipped redaction rules; no provider is cleared by this policy alone.

        Provider clearances are a deployment decision, so a caller uses
        [`DlpPolicy.from_catalog`] for the catalog's own providers or supplies explicit clearances.
        """
        return cls(
            redactions=(
                Redaction(
                    rule_id="dlp.redaction.secret_like",
                    pattern=re.compile(
                        r"(?:sk-[A-Za-z0-9]{8,}|gh[pousr]_[A-Za-z0-9]{8,}|AKIA[0-9A-Z]{8,}"
                        r"|-----BEGIN [A-Z ]*PRIVATE KEY-----)"
                    ),
                    placeholder="[REDACTED:secret]",
                ),
                Redaction(
                    rule_id="dlp.redaction.email",
                    pattern=re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
                    placeholder="[REDACTED:email]",
                ),
            )
        )


@dataclass(frozen=True, slots=True)
class DlpDecision:
    """What the guard decided, with the rule that decided it."""

    allowed: bool
    rule_id: str
    data_class: DataClass
    fired_redactions: tuple[str, ...] = ()

    def describe(self) -> str:
        """A one-line explanation suitable for a log or a refusal."""
        return f"{self.rule_id}: data_class={self.data_class} redactions={list(self.fired_redactions)}"


class DlpGuard:
    """Enforce data policy before a request reaches a provider."""

    __slots__ = ("_policy",)

    def __init__(self, policy: DlpPolicy | None = None) -> None:
        self._policy = policy or DlpPolicy.builtin()

    @property
    def policy(self) -> DlpPolicy:
        return self._policy

    def check(
        self,
        request: intelligence_pb2.ModelCallRequest,
        model: ModelSpec,
        provider: ProviderConfig,
    ) -> DlpDecision:
        """Decide whether this request may go to this provider, or refuse it.

        Raises `GatewayError(DLP_DENIED)` before anything is transmitted when the combination is
        not allowed (acceptance 2).
        """
        data_class = self._policy.data_class_for(request.dlp_profile)
        fired = tuple(
            rule.rule_id
            for rule in self._policy.redactions
            if any(rule.pattern.search(_message_text(message)) for message in request.messages)
        )
        if not model.dlp_eligible:
            raise GatewayError(
                GatewayErrorCode.DLP_DENIED,
                f"model '{model.id}' is not cleared for {data_class} content (rule {RULE_MODEL_CLEARANCE})",
            )
        allowed = self._policy.allowed_by_provider.get(provider.name, frozenset())
        if data_class not in allowed:
            raise GatewayError(
                GatewayErrorCode.DLP_DENIED,
                f"provider '{provider.name}' is not cleared for {data_class} content "
                f"(rule {RULE_PROVIDER_CLEARANCE})",
            )
        rule_id = RULE_REDACTION if fired else RULE_DATA_CLASS
        return DlpDecision(
            allowed=True,
            rule_id=rule_id,
            data_class=data_class,
            fired_redactions=fired,
        )

    def redact(self, request: intelligence_pb2.ModelCallRequest) -> tuple[int, ...]:
        """Redact the request in place, returning the indexes of messages it changed.

        Only the content that is transmitted is rewritten: the caller's own copy of the prompt is
        never mutated, and the returned indexes make the redaction auditable.
        """
        changed: list[int] = []
        for index, message in enumerate(request.messages):
            text = _message_text(message)
            redacted = text
            for rule in self._policy.redactions:
                redacted = rule.pattern.sub(rule.placeholder, redacted)
            if redacted != text:
                message.content_json = _json_string(redacted)
                changed.append(index)
        return tuple(changed)


def _message_text(message: intelligence_pb2.RenderedMessage) -> str:
    """The text of a rendered message, whatever shape its content carries."""
    import json

    try:
        parsed = json.loads(message.content_json or "null")
    except json.JSONDecodeError:  # pragma: no cover - malformed content is passed through
        return message.content_json or ""
    return parsed if isinstance(parsed, str) else json.dumps(parsed, sort_keys=True)


def _json_string(value: str) -> str:
    import json

    return json.dumps(value)


def redaction_summary(rules: Sequence[str]) -> str:
    """A stable summary of the redaction rules that fired, for events and evidence."""
    return ",".join(sorted(set(rules)))
