"""Bounded conversation projection: what a summary may carry, and the budget it is packed under
(INT-008, DOMAIN.md §5.7, DOSSIER.md §8).

A long conversation is bounded by summarising its older part into a compaction epoch and projecting the
summary plus the recent turns. `summary` owns both rules that make that safe: a summary carries a closed
vocabulary of *narrative* sections and can never carry protocol truth, and a projection never presents a
summary of history the caller's lineage no longer contains (nor truncates one to fit a budget).

The epoch lifecycle itself — create, install, refuse-as-stale — is the Rust half
(`crates/server/src/runtime/compaction/`), because an epoch is runtime state; this package is what a
summary is *allowed to say* and how the bounded projection is assembled from it.
"""

from __future__ import annotations

from intelligence.context.compaction.summary import (
    FORBIDDEN_SECTIONS,
    BoundedProjection,
    CompactionSummary,
    SummarySection,
    SummarySectionKind,
    project_conversation,
)

__all__ = [
    "FORBIDDEN_SECTIONS",
    "BoundedProjection",
    "CompactionSummary",
    "SummarySection",
    "SummarySectionKind",
    "project_conversation",
]
