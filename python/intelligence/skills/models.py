"""Skills as versioned procedural assets (DOMAIN.md §11.5, INT-009).

A skill is a *procedure*, not a subsystem: it may guide how work is done, and it may not grant a
permission, add a tool or widen authority. Two rules follow from that and are enforced here:

* **Only `ACTIVE` versions resolve.** `APPROVED` is a review outcome, not a production state, so an
  approved-but-not-active version is refused exactly like a draft — the distinction DOMAIN.md
  §11.5 draws between the review ladder and the production state.
* **A manifest is data with a closed vocabulary.** An unknown key, an unknown status or a malformed
  need is refused when the version is read, so a skill cannot smuggle behaviour through a field the
  resolver does not understand.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from enum import StrEnum


class SkillError(ValueError):
    """A refused skill artefact, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


RULE_STATUS = "skill.status"
RULE_MANIFEST_KEYS = "skill.manifest_keys"
RULE_MANIFEST_SHAPE = "skill.manifest_shape"
RULE_SNAPSHOT = "skill.snapshot"

#: The §11.5 ladder, in order. `REJECTED` is reachable from `EVALUATING` only.
SKILL_STATES: tuple[str, ...] = (
    "draft",
    "candidate",
    "evaluating",
    "approved",
    "active",
    "deprecated",
    "retired",
    "rejected",
)

#: The only state whose versions resolve into production context.
RESOLVABLE_STATE = "active"

MANIFEST_KEYS: frozenset[str] = frozenset(
    {
        "instructions",
        "examples",
        "tool_needs",
        "capability_needs",
        "eval_suite_id",
        "compatibility",
        "recovery_guidance",
    }
)


class SkillStatus(StrEnum):
    """The review and production state of one skill version."""

    DRAFT = "draft"
    CANDIDATE = "candidate"
    EVALUATING = "evaluating"
    APPROVED = "approved"
    ACTIVE = "active"
    DEPRECATED = "deprecated"
    RETIRED = "retired"
    REJECTED = "rejected"

    @classmethod
    def parse(cls, value: str) -> SkillStatus:
        """Parse a status, refusing anything outside the §11.5 ladder."""
        try:
            return cls(value.strip().lower())
        except ValueError as error:
            raise SkillError("VALIDATION_SCHEMA", RULE_STATUS, f"{value!r} is not a skill status") from error

    @property
    def resolves(self) -> bool:
        """Whether versions in this state may enter production context."""
        return self is SkillStatus.ACTIVE

    @property
    def previous(self) -> tuple[SkillStatus, ...]:
        """The states a version may be promoted from (the §11.5 ladder)."""
        match self:
            case SkillStatus.CANDIDATE:
                return (SkillStatus.DRAFT,)
            case SkillStatus.EVALUATING:
                return (SkillStatus.CANDIDATE,)
            case SkillStatus.APPROVED:
                return (SkillStatus.EVALUATING,)
            case SkillStatus.ACTIVE:
                return (SkillStatus.APPROVED,)
            case SkillStatus.DEPRECATED:
                return (SkillStatus.ACTIVE,)
            case SkillStatus.RETIRED:
                return (SkillStatus.DEPRECATED,)
            case SkillStatus.REJECTED:
                return (SkillStatus.EVALUATING,)
            case SkillStatus.DRAFT:
                return ()


@dataclass(frozen=True, slots=True)
class CapabilitySnapshot:
    """What the caller may already do: the only authority a skill runs under."""

    tool_names: frozenset[str] = frozenset()
    capability_needs: frozenset[str] = frozenset()

    def permits_tool(self, tool: str) -> bool:
        return tool in self.tool_names

    def permits_capability(self, need: str) -> bool:
        return need in self.capability_needs


@dataclass(frozen=True, slots=True)
class SkillManifest:
    """A version's manifest (DOMAIN.md §11.5)."""

    instructions: str = ""
    examples: tuple[str, ...] = ()
    tool_needs: tuple[str, ...] = ()
    capability_needs: tuple[str, ...] = ()
    eval_suite_id: str | None = None
    compatibility: tuple[str, ...] = ()
    recovery_guidance: str | None = None

    @classmethod
    def parse(cls, payload: Mapping[str, object] | str | None) -> SkillManifest:
        """Parse a manifest, refusing unknown keys and malformed needs.

        Raises [`SkillError`] (`VALIDATION_SCHEMA`).
        """
        if payload is None:
            return cls()
        raw = json.loads(payload) if isinstance(payload, str) else dict(payload)
        unknown = sorted(set(raw) - MANIFEST_KEYS)
        if unknown:
            raise SkillError(
                "VALIDATION_SCHEMA",
                RULE_MANIFEST_KEYS,
                f"unknown manifest keys: {', '.join(unknown)}",
            )
        instructions = raw.get("instructions", "")
        if not isinstance(instructions, str):
            raise SkillError("VALIDATION_SCHEMA", RULE_MANIFEST_SHAPE, "instructions must be text")
        return cls(
            instructions=instructions,
            examples=_string_tuple(raw.get("examples"), "examples"),
            tool_needs=_string_tuple(raw.get("tool_needs"), "tool_needs"),
            capability_needs=_string_tuple(raw.get("capability_needs"), "capability_needs"),
            eval_suite_id=_optional_string(raw.get("eval_suite_id"), "eval_suite_id"),
            compatibility=_string_tuple(raw.get("compatibility"), "compatibility"),
            recovery_guidance=_optional_string(raw.get("recovery_guidance"), "recovery_guidance"),
        )

    @property
    def triggers(self) -> tuple[str, ...]:
        """The phrases that make this skill relevant, taken from its declared examples."""
        return self.examples


def _string_tuple(value: object, field_name: str) -> tuple[str, ...]:
    if value is None:
        return ()
    if not isinstance(value, Sequence) or isinstance(value, (str, bytes)):
        raise SkillError("VALIDATION_SCHEMA", RULE_MANIFEST_SHAPE, f"{field_name} must be a list of strings")
    for item in value:
        if not isinstance(item, str) or not item.strip():
            raise SkillError(
                "VALIDATION_SCHEMA", RULE_MANIFEST_SHAPE, f"{field_name} must hold non-empty strings"
            )
    return tuple(str(item) for item in value)


def _optional_string(value: object, field_name: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or not value.strip():
        raise SkillError("VALIDATION_SCHEMA", RULE_MANIFEST_SHAPE, f"{field_name} must be a non-empty string")
    return value


@dataclass(frozen=True, slots=True)
class SkillVersionRef:
    """A version as the resolver sees it, straight from the control plane."""

    skill_id: str
    version_id: str
    skill_name: str
    semver: str
    status: SkillStatus
    manifest: SkillManifest = field(default_factory=SkillManifest)
    provenance: str | None = None

    @classmethod
    def build(
        cls,
        *,
        skill_id: str,
        version_id: str,
        skill_name: str,
        semver: str,
        status: object,
        manifest: Mapping[str, object] | str | None = None,
        provenance: str | None = None,
    ) -> SkillVersionRef:
        """Build a version reference, refusing an unknown status or malformed manifest."""
        if not isinstance(status, SkillStatus):
            if not isinstance(status, str) or not status.strip():
                raise SkillError("VALIDATION_SCHEMA", RULE_STATUS, "a version needs a status")
            status = SkillStatus.parse(status)
        for name, value in (
            ("skill_id", skill_id),
            ("version_id", version_id),
            ("skill_name", skill_name),
            ("semver", semver),
        ):
            if not isinstance(value, str) or not value.strip():
                raise SkillError("VALIDATION_SCHEMA", RULE_MANIFEST_SHAPE, f"a version needs a {name}")
        return cls(
            skill_id=skill_id,
            version_id=version_id,
            skill_name=skill_name,
            semver=semver,
            status=status,
            manifest=SkillManifest.parse(manifest),
            provenance=provenance,
        )
