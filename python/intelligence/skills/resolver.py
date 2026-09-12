"""Task-scoped skill resolution (INT-009, DOMAIN.md §11.5).

Resolution answers one question: *which approved, active skills are relevant to this task and
already permitted by the caller's own authority?* It is deliberately boring, because the properties
it must have are the ones that keep skills from becoming a way around the runtime:

* **Only `ACTIVE` versions resolve.** Anything else is excluded with the rule that excluded it, so
  an approval that never reached production cannot leak into context.
* **Skills cannot widen authority.** A version whose `tool_needs` or `capability_needs` exceed the
  caller's snapshot is excluded — never granted — so resolution can only narrow what the caller may
  already do.
* **Bounded and deterministic.** At most `max_skills` resolve, and the result is a pure function of
  the inputs: the same task, snapshot and candidate set always resolve to the same skills in the
  same order, whatever order the candidates arrived in.
"""

from __future__ import annotations

from dataclasses import dataclass

from intelligence.skills.models import CapabilitySnapshot, SkillVersionRef

RULE_NOT_ACTIVE = "skill.not_active"
RULE_TOOL_NOT_AVAILABLE = "skill.tool_not_available"
RULE_CAPABILITY_NOT_GRANTED = "skill.capability_not_granted"
RULE_IRRELEVANT = "skill.irrelevant"
RULE_BUDGET = "resolver.budget"

#: Why one candidate did not resolve, with the rule that excluded it.
WEIGHT_TRIGGER_MATCH = 2.0
WEIGHT_NAME_MATCH = 1.0


@dataclass(frozen=True, slots=True)
class ExcludedSkill:
    """A candidate that did not resolve, and why."""

    skill_id: str
    version_id: str
    rule_id: str
    detail: str


@dataclass(frozen=True, slots=True)
class Resolution:
    """What resolved, what did not, and the authority the result runs under."""

    resolved: tuple[SkillVersionRef, ...]
    excluded: tuple[ExcludedSkill, ...]
    snapshot: CapabilitySnapshot
    budget: int

    @property
    def budget_used(self) -> int:
        return len(self.resolved)

    def resolved_ids(self) -> tuple[str, ...]:
        return tuple(version.version_id for version in self.resolved)

    def rules_fired(self) -> tuple[str, ...]:
        """The distinct exclusion rules, sorted, for reporting."""
        return tuple(sorted({excluded.rule_id for excluded in self.excluded}))


def _relevance(version: SkillVersionRef, task_text: str) -> float:
    """How relevant a version is to the task: a declared trigger, then its name."""
    haystack = task_text.lower()
    score = 0.0
    for trigger in version.manifest.triggers:
        if trigger.lower() in haystack:
            score += WEIGHT_TRIGGER_MATCH
    if version.skill_name.lower().replace("-", " ") in haystack:
        score += WEIGHT_NAME_MATCH
    return score


def resolve(
    *,
    task_text: str,
    snapshot: CapabilitySnapshot,
    candidates: tuple[SkillVersionRef, ...] | list[SkillVersionRef],
    max_skills: int = 3,
) -> Resolution:
    """Resolve the skills a task may use, within the caller's own authority.

    Raises `ValueError` for a nonsensical budget; every candidate is accounted for either in
    `resolved` or in `excluded`.
    """
    if max_skills < 0:
        raise ValueError("max_skills must not be negative")
    eligible: list[tuple[float, SkillVersionRef]] = []
    excluded: list[ExcludedSkill] = []
    for version in candidates:
        # 1. Production state: only ACTIVE versions resolve.
        if not version.status.resolves:
            excluded.append(
                ExcludedSkill(
                    skill_id=version.skill_id,
                    version_id=version.version_id,
                    rule_id=RULE_NOT_ACTIVE,
                    detail=f"version is {version.status.value}, not active",
                )
            )
            continue
        # 2. Authority: the skill may not need anything the caller does not already have.
        missing_tool = next(
            (tool for tool in version.manifest.tool_needs if not snapshot.permits_tool(tool)), None
        )
        if missing_tool is not None:
            excluded.append(
                ExcludedSkill(
                    skill_id=version.skill_id,
                    version_id=version.version_id,
                    rule_id=RULE_TOOL_NOT_AVAILABLE,
                    detail=f"needs tool {missing_tool!r}, which the caller does not hold",
                )
            )
            continue
        missing_need = next(
            (need for need in version.manifest.capability_needs if not snapshot.permits_capability(need)),
            None,
        )
        if missing_need is not None:
            excluded.append(
                ExcludedSkill(
                    skill_id=version.skill_id,
                    version_id=version.version_id,
                    rule_id=RULE_CAPABILITY_NOT_GRANTED,
                    detail=f"needs capability {missing_need!r}, which is not in the snapshot",
                )
            )
            continue
        # 3. Relevance to this task.
        score = _relevance(version, task_text)
        if score <= 0.0:
            excluded.append(
                ExcludedSkill(
                    skill_id=version.skill_id,
                    version_id=version.version_id,
                    rule_id=RULE_IRRELEVANT,
                    detail="no declared example or name matches the task",
                )
            )
            continue
        eligible.append((score, version))

    # Deterministic order: relevance desc, then skill name, then semver, then version id.
    eligible.sort(
        key=lambda entry: (
            -entry[0],
            entry[1].skill_name,
            entry[1].semver,
            entry[1].version_id,
        )
    )
    resolved = tuple(version for _, version in eligible[:max_skills])
    for _, version in eligible[max_skills:]:
        excluded.append(
            ExcludedSkill(
                skill_id=version.skill_id,
                version_id=version.version_id,
                rule_id=RULE_BUDGET,
                detail=f"the resolver budget of {max_skills} was already used",
            )
        )
    excluded.sort(key=lambda entry: (entry.skill_id, entry.version_id))
    return Resolution(
        resolved=resolved,
        excluded=tuple(excluded),
        snapshot=snapshot,
        budget=max_skills,
    )
