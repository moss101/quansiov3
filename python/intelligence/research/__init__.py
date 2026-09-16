"""Evidence-first research (CAP-001).

Composes SearchProgram, ContextProjection and digest-addressed evidence
descriptors. No second orchestrator, no second memory store.
"""

from __future__ import annotations

from .workflow import (
    STAGES,
    Claim,
    EvidenceDescriptor,
    ResearchAnswer,
    ResearchError,
    ResearchPlan,
    ResearchRun,
    SearchBackend,
    collect,
    collected_segments,
    compile_intent,
    synthesize,
)

__all__ = [
    "STAGES",
    "Claim",
    "EvidenceDescriptor",
    "ResearchAnswer",
    "ResearchError",
    "ResearchPlan",
    "ResearchRun",
    "SearchBackend",
    "collect",
    "collected_segments",
    "compile_intent",
    "synthesize",
]
