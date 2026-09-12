"""Data-classification and redaction for the model gateway (INT-003, DOMAIN.md §12).

Canonical owner: `python/intelligence/model_gateway/dlp`. The guard decides whether a request's
data class may reach a provider at all — before anything is transmitted — and rewrites the
content that is transmitted so a secret-shaped value never leaves in the first place.
"""

from __future__ import annotations

from intelligence.model_gateway.dlp.guard import (
    DATA_CLASSES,
    DataClass,
    DlpDecision,
    DlpGuard,
    DlpPolicy,
    Redaction,
    redaction_summary,
)

__all__ = [
    "DATA_CLASSES",
    "DataClass",
    "DlpDecision",
    "DlpGuard",
    "DlpPolicy",
    "Redaction",
    "redaction_summary",
]
