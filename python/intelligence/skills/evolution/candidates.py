"""Candidate skill patches: proposed, evaluated, never silently applied (CAP-008).

A task run may *propose* a patch whose provenance names the evidence pattern and the
evaluation run. It may not write an ACTIVE skill. Promotion is a control-plane
decision after the INT-010 gate passes; a protected regression refuses the proposal.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, replace

from intelligence.evaluation.gate import ThresholdSet, evaluate_gate
from intelligence.evaluation.runs import EvaluationRun
from intelligence.skills.evolution.patterns import EvolutionError, Pattern
from intelligence.skills.models import SkillManifest, SkillStatus, SkillVersionRef

RULE_SILENT = "evolution.silent_mutation"
RULE_PROVENANCE = "evolution.provenance"
RULE_PROMOTION = "evolution.promotion"
RULE_REGRESSION = "evolution.regression"


@dataclass(frozen=True, slots=True)
class CandidatePatch:
    """A proposed skill version. Status is always candidate — never production."""

    skill_id: str
    from_version_id: str
    proposed: SkillVersionRef
    evidence_ids: tuple[str, ...]
    evidence_digests: tuple[str, ...]
    eval_run_id: str | None = None
    pattern_kind: str = ""

    def __post_init__(self) -> None:
        if self.proposed.status is SkillStatus.ACTIVE:
            raise EvolutionError(
                "RUNTIME_ILLEGAL_TRANSITION",
                RULE_SILENT,
                "a candidate patch cannot be ACTIVE; promotion is a separate governed step",
            )
        if self.proposed.status is not SkillStatus.CANDIDATE:
            raise EvolutionError(
                "VALIDATION_SCHEMA",
                RULE_PROMOTION,
                f"a patch must be candidate, not {self.proposed.status.value}",
            )
        if not self.evidence_ids or not self.evidence_digests:
            raise EvolutionError(
                "VALIDATION_SCHEMA",
                RULE_PROVENANCE,
                "a candidate needs evidence identities and digests linking back to CORE-007",
            )
        if self.proposed.skill_id != self.skill_id:
            raise EvolutionError("VALIDATION_SCHEMA", RULE_PROMOTION, "patch skill_id does not match")

    @property
    def provenance(self) -> Mapping[str, object]:
        return {
            "evidence_ids": list(self.evidence_ids),
            "evidence_digests": list(self.evidence_digests),
            "eval_run_id": self.eval_run_id,
            "from_version_id": self.from_version_id,
            "pattern_kind": self.pattern_kind,
        }


@dataclass(frozen=True, slots=True)
class PromotionProposal:
    """What the control plane may promote. This module never writes production state."""

    skill_id: str
    from_version_id: str
    to_version_id: str
    eval_run_id: str
    evidence_ids: tuple[str, ...]
    evidence_digests: tuple[str, ...]
    gate_reason: str


def propose_from_pattern(
    current: SkillVersionRef,
    pattern: Pattern,
    *,
    recovery_guidance: str,
    version_id: str,
) -> CandidatePatch:
    """Mint a candidate whose instructions/recovery come from repeated verified evidence."""
    if current.skill_id != pattern.skill_id:
        raise EvolutionError("VALIDATION_SCHEMA", RULE_PROMOTION, "pattern skill does not match current")
    if not pattern.addressable:
        raise EvolutionError("VALIDATION_SCHEMA", RULE_PROVENANCE, "pattern has no evidence address")
    manifest = SkillManifest.parse(
        {
            "instructions": current.manifest.instructions,
            "examples": list(current.manifest.examples),
            "tool_needs": list(current.manifest.tool_needs),
            "capability_needs": list(current.manifest.capability_needs),
            "eval_suite_id": current.manifest.eval_suite_id,
            "compatibility": list(current.manifest.compatibility),
            "recovery_guidance": recovery_guidance,
        }
    )
    proposed = SkillVersionRef(
        skill_id=current.skill_id,
        version_id=version_id,
        skill_name=current.skill_name,
        semver=_bump_patch(current.semver),
        status=SkillStatus.CANDIDATE,
        manifest=manifest,
        provenance=f"evidence:{pattern.evidence_ids[0]}",
    )
    return CandidatePatch(
        skill_id=current.skill_id,
        from_version_id=current.version_id,
        proposed=proposed,
        evidence_ids=pattern.evidence_ids,
        evidence_digests=pattern.digests,
        pattern_kind=pattern.kind.value,
    )


def bind_evaluation(patch: CandidatePatch, run: EvaluationRun) -> CandidatePatch:
    """Attach the eval run identity so provenance names both evidence and evaluation."""
    return replace(patch, eval_run_id=run.run_id)


def apply_to_production(
    current: SkillVersionRef,
    patch: CandidatePatch,
) -> SkillVersionRef:
    """Refuse. A task run cannot change the production skill (acceptance 1)."""
    raise EvolutionError(
        "RUNTIME_ILLEGAL_TRANSITION",
        RULE_SILENT,
        f"production skill {current.skill_id} cannot change directly from a task run "
        f"(attempted {patch.proposed.version_id}); use governed promotion after evaluation",
    )


def propose_promotion(
    patch: CandidatePatch,
    run: EvaluationRun,
    thresholds: ThresholdSet,
) -> PromotionProposal:
    """Evaluate then, only if the gate passes, emit a promotion proposal. Never mutates."""
    if patch.eval_run_id != run.run_id:
        raise EvolutionError(
            "VALIDATION_SCHEMA",
            RULE_PROVENANCE,
            "candidate eval_run_id must match the evaluation run that scored it",
        )
    verdict = evaluate_gate(run, thresholds)
    if verdict.promotion_blocked or not verdict.passed:
        raise EvolutionError(
            "POLICY_DENIED",
            RULE_REGRESSION,
            f"regression/safety gate refused promotion: {verdict.reason}",
        )
    if not patch.evidence_ids or not patch.eval_run_id:
        raise EvolutionError("VALIDATION_SCHEMA", RULE_PROVENANCE, "promotion needs evidence and eval")
    return PromotionProposal(
        skill_id=patch.skill_id,
        from_version_id=patch.from_version_id,
        to_version_id=patch.proposed.version_id,
        eval_run_id=run.run_id,
        evidence_ids=patch.evidence_ids,
        evidence_digests=patch.evidence_digests,
        gate_reason=verdict.reason,
    )


def _bump_patch(semver: str) -> str:
    parts = semver.split(".")
    if len(parts) != 3 or not all(part.isdigit() for part in parts):
        raise EvolutionError(
            "VALIDATION_SCHEMA", RULE_PROMOTION, f"semver {semver!r} is not MAJOR.MINOR.PATCH"
        )
    return f"{parts[0]}.{parts[1]}.{int(parts[2]) + 1}"
