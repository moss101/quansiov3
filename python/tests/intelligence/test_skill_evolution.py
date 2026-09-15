"""CAP-008: production skills cannot change from a task run; candidates carry evidence+eval."""

from __future__ import annotations

import pytest

from intelligence.evaluation.datasets import dataset_from_mapping
from intelligence.evaluation.gate import GateThreshold, ThresholdSet
from intelligence.evaluation.runs import CostLatency, ImplementationVersions, Measurement, run_for
from intelligence.skills.evolution import (
    EvidenceKind,
    EvidenceRecord,
    EvolutionError,
    apply_to_production,
    bind_evaluation,
    detect_patterns,
    propose_from_pattern,
    propose_promotion,
)
from intelligence.skills.models import SkillStatus, SkillVersionRef

DIGEST_A = "a" * 64
DIGEST_B = "b" * 64
DIGEST_C = "c" * 64


def _current() -> SkillVersionRef:
    return SkillVersionRef.build(
        skill_id="skl_coding",
        version_id="sklv_coding_1",
        skill_name="coding",
        semver="1.0.0",
        status="active",
        manifest={
            "instructions": "patch then open a pr",
            "examples": ["implement the change"],
            "tool_needs": ["fs.patch"],
            "capability_needs": ["scm.write"],
            "eval_suite_id": "eval_coding",
            "recovery_guidance": "stop on unknown effects",
        },
    )


def _records() -> tuple[EvidenceRecord, ...]:
    return tuple(
        EvidenceRecord(
            evidence_id=f"evd_{label}",
            skill_id="skl_coding",
            run_id=f"run_{label}",
            kind=EvidenceKind.RECOVERY,
            digest=digest,
            detail="recovered after OUTCOME_UNKNOWN",
        )
        for label, digest in (("aaa", DIGEST_A), ("bbb", DIGEST_B), ("ccc", DIGEST_C))
    )


def _thresholds() -> ThresholdSet:
    return ThresholdSet(
        version=1,
        ratified=False,
        source="CAP-008 evolution gate",
        thresholds={
            "protected_recovery_safety_regressions": GateThreshold(
                metric="protected_recovery_safety_regressions",
                comparison="at_most",
                threshold=0.0,
                unit="count",
                protected=True,
                blocks_promotion=True,
            ),
            "skill_resolution_accuracy": GateThreshold(
                metric="skill_resolution_accuracy",
                comparison="at_least",
                threshold=1.0,
                unit="ratio",
            ),
        },
    )


def _run(*, regressions: float, accuracy: float, run_id: str = "eval_evo_1") -> object:
    dataset = dataset_from_mapping(
        {
            "dataset_id": "evolution_behaviour",
            "version": 1,
            "kind": "skill_behaviour",
            "cases": [
                {"case_id": "recovery", "payload": {"kind": "recovery"}, "protected": True, "tags": []}
            ],
        }
    )
    return run_for(
        run_id,
        [dataset],
        measurements=(
            Measurement(metric="protected_recovery_safety_regressions", value=regressions, unit="count"),
            Measurement(metric="skill_resolution_accuracy", value=accuracy, unit="ratio"),
        ),
        versions=ImplementationVersions(skill_versions=(("skl_coding", "sklv_coding_1"),)),
        cost=CostLatency(wall_time_ms=10),
    )


def test_silent_mutation_from_a_task_run_is_refused() -> None:
    """Named test: silent mutation negative. apply_to_production cannot change ACTIVE."""
    current = _current()
    patterns = detect_patterns(_records())
    assert len(patterns) == 1
    assert patterns[0].kind is EvidenceKind.RECOVERY
    assert patterns[0].evidence_ids == ("evd_aaa", "evd_bbb", "evd_ccc")
    patch = propose_from_pattern(
        current,
        patterns[0],
        recovery_guidance="reconcile OUTCOME_UNKNOWN before retry",
        version_id="sklv_coding_2",
    )
    assert patch.proposed.status is SkillStatus.CANDIDATE
    assert current.status is SkillStatus.ACTIVE
    with pytest.raises(EvolutionError) as raised:
        apply_to_production(current, patch)
    assert raised.value.rule_id == "evolution.silent_mutation"
    assert "cannot change directly from a task run" in raised.value.detail
    # The live version is untouched.
    assert current.version_id == "sklv_coding_1"
    assert current.status is SkillStatus.ACTIVE


def test_candidate_to_promotion_keeps_evidence_and_eval_provenance() -> None:
    """Named test: candidate-to-promotion. Provenance names evidence and the eval run."""
    current = _current()
    patch = propose_from_pattern(
        current,
        detect_patterns(_records())[0],
        recovery_guidance="reconcile OUTCOME_UNKNOWN before retry",
        version_id="sklv_coding_2",
    )
    run = _run(regressions=0, accuracy=1.0)
    bound = bind_evaluation(patch, run)
    assert bound.provenance["evidence_ids"] == ["evd_aaa", "evd_bbb", "evd_ccc"]
    assert bound.provenance["eval_run_id"] == "eval_evo_1"
    proposal = propose_promotion(bound, run, _thresholds())
    assert proposal.skill_id == "skl_coding"
    assert proposal.from_version_id == "sklv_coding_1"
    assert proposal.to_version_id == "sklv_coding_2"
    assert proposal.eval_run_id == "eval_evo_1"
    assert proposal.evidence_ids == bound.evidence_ids
    assert proposal.evidence_digests == (DIGEST_A, DIGEST_B, DIGEST_C)
    # Still not applied: production identity is unchanged.
    assert current.status is SkillStatus.ACTIVE
    assert current.version_id != proposal.to_version_id


def test_regression_rejects_promotion() -> None:
    """Named test: regression rejection. A protected regression is not promotable."""
    current = _current()
    patch = bind_evaluation(
        propose_from_pattern(
            current,
            detect_patterns(_records())[0],
            recovery_guidance="reconcile OUTCOME_UNKNOWN before retry",
            version_id="sklv_coding_2",
        ),
        _run(regressions=1, accuracy=1.0, run_id="eval_evo_bad"),
    )
    with pytest.raises(EvolutionError) as raised:
        propose_promotion(patch, _run(regressions=1, accuracy=1.0, run_id="eval_evo_bad"), _thresholds())
    assert raised.value.rule_id == "evolution.regression"
    assert "refused promotion" in raised.value.detail
    with pytest.raises(EvolutionError) as raised:
        apply_to_production(current, patch)
    assert raised.value.rule_id == "evolution.silent_mutation"
