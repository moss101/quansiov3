"""Detect repeated verified success, failure and recovery from evidence (CAP-008).

A pattern is only a candidate generator when the same skill shows the same
verified outcome enough times to be more than one noisy run. Evidence identities
are required: a pattern with no evidence is an anecdote, not experience.
"""

from __future__ import annotations

from collections import defaultdict
from collections.abc import Sequence
from dataclasses import dataclass
from enum import StrEnum

RULE_EVIDENCE = "evolution.evidence"
RULE_PATTERN = "evolution.pattern"
MIN_REPEATS = 3
PATTERN_KINDS: tuple[str, ...] = ("success", "failure", "recovery")


class EvolutionError(ValueError):
    """A refused evolution step, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


class EvidenceKind(StrEnum):
    SUCCESS = "success"
    FAILURE = "failure"
    RECOVERY = "recovery"

    @classmethod
    def parse(cls, value: object) -> EvidenceKind:
        if not isinstance(value, str) or value not in PATTERN_KINDS:
            raise EvolutionError("VALIDATION_SCHEMA", RULE_EVIDENCE, f"{value!r} is not an evidence kind")
        return cls(value)


@dataclass(frozen=True, slots=True)
class EvidenceRecord:
    """One verified outcome bound to a skill, a run and an evidence identity (CORE-007)."""

    evidence_id: str
    skill_id: str
    run_id: str
    kind: EvidenceKind
    digest: str
    detail: str = ""

    def __post_init__(self) -> None:
        if not self.evidence_id.startswith("evd_"):
            raise EvolutionError("VALIDATION_SCHEMA", RULE_EVIDENCE, "evidence_id must start with evd_")
        if not self.skill_id.startswith("skl_"):
            raise EvolutionError("VALIDATION_SCHEMA", RULE_EVIDENCE, "skill_id must start with skl_")
        if not self.run_id.startswith("run_"):
            raise EvolutionError("VALIDATION_SCHEMA", RULE_EVIDENCE, "run_id must start with run_")
        if len(self.digest) != 64:
            raise EvolutionError("VALIDATION_SCHEMA", RULE_EVIDENCE, "evidence digest must be sha256")


@dataclass(frozen=True, slots=True)
class Pattern:
    """A repeated verified outcome for one skill, addressed by the evidence that showed it."""

    skill_id: str
    kind: EvidenceKind
    count: int
    evidence_ids: tuple[str, ...]
    digests: tuple[str, ...]
    run_ids: tuple[str, ...]

    @property
    def addressable(self) -> bool:
        return bool(self.evidence_ids) and bool(self.digests)


def detect_patterns(
    records: Sequence[EvidenceRecord],
    *,
    min_repeats: int = MIN_REPEATS,
) -> tuple[Pattern, ...]:
    """Group verified outcomes. Fewer than `min_repeats` is not a pattern."""
    if min_repeats < 2:
        raise EvolutionError("VALIDATION_BOUNDS", RULE_PATTERN, "min_repeats must be at least 2")
    buckets: dict[tuple[str, EvidenceKind], list[EvidenceRecord]] = defaultdict(list)
    for record in records:
        buckets[(record.skill_id, record.kind)].append(record)
    patterns: list[Pattern] = []
    for (skill_id, kind), group in sorted(buckets.items(), key=lambda item: (item[0][0], item[0][1].value)):
        if len(group) < min_repeats:
            continue
        patterns.append(
            Pattern(
                skill_id=skill_id,
                kind=kind,
                count=len(group),
                evidence_ids=tuple(item.evidence_id for item in group),
                digests=tuple(item.digest for item in group),
                run_ids=tuple(item.run_id for item in group),
            )
        )
    return tuple(patterns)
