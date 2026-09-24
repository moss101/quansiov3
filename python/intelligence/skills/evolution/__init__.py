"""Controlled skill evolution (CAP-008).

Detect repeated verified outcomes, propose candidate patches with evidence+eval
provenance, and refuse any silent write of an ACTIVE skill. Promotion is a
proposal for the control plane after the INT-010 gate passes.
"""

from __future__ import annotations

from intelligence.skills.evolution.candidates import (
    CandidatePatch,
    PromotionProposal,
    apply_to_production,
    bind_evaluation,
    propose_from_pattern,
    propose_promotion,
)
from intelligence.skills.evolution.evaluate import (
    CandidateEvaluation,
    SkillCase,
    default_thresholds,
    evaluate_candidate,
    synthesized_suite,
)
from intelligence.skills.evolution.patterns import (
    MIN_REPEATS,
    PATTERN_KINDS,
    EvidenceKind,
    EvidenceRecord,
    EvolutionError,
    Pattern,
    detect_patterns,
)
from intelligence.skills.evolution.propose import EvolutionProposal, propose_candidates

__all__ = [
    "MIN_REPEATS",
    "PATTERN_KINDS",
    "CandidateEvaluation",
    "CandidatePatch",
    "EvidenceKind",
    "EvidenceRecord",
    "EvolutionError",
    "EvolutionProposal",
    "Pattern",
    "PromotionProposal",
    "SkillCase",
    "apply_to_production",
    "bind_evaluation",
    "default_thresholds",
    "detect_patterns",
    "evaluate_candidate",
    "propose_candidates",
    "propose_from_pattern",
    "propose_promotion",
    "synthesized_suite",
]
