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
from intelligence.skills.evolution.patterns import (
    MIN_REPEATS,
    PATTERN_KINDS,
    EvidenceKind,
    EvidenceRecord,
    EvolutionError,
    Pattern,
    detect_patterns,
)

__all__ = [
    "MIN_REPEATS",
    "PATTERN_KINDS",
    "CandidatePatch",
    "EvidenceKind",
    "EvidenceRecord",
    "EvolutionError",
    "Pattern",
    "PromotionProposal",
    "apply_to_production",
    "bind_evaluation",
    "detect_patterns",
    "propose_from_pattern",
    "propose_promotion",
]
