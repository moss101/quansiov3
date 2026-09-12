"""Content trust labelling and deterministic injection defense (INT-012, DOMAIN.md §12).

Canonical owner: `python/intelligence/trust`. Fetched pages, emails, documents, file contents and
tool output are labelled by their source, rendered inside a typed boundary with a data-only
instruction when their origin is not trustworthy, and scanned by deterministic (non-LLM) heuristics
that tag suspected injection, replace the suspect text with an evidence reference and surface it.

The policy-side half — escalation, `always`-rule refusal and the exfiltration guard — lives in the
runtime's policy layer (INT-012's Rust half).
"""

from __future__ import annotations

from intelligence.trust.injection import (
    PATTERNS,
    Assessment,
    InjectionError,
    SuspectedSegment,
    assess_all,
    assess_segment,
)
from intelligence.trust.labelling import (
    BOUNDARY_CLOSE,
    BOUNDARY_OPEN,
    DATA_ONLY_INSTRUCTION,
    SOURCE_TRUST,
    TRUST_LEVELS,
    LabelledSegment,
    TrustLevel,
    label_for_source,
)
from intelligence.trust.propagation import (
    Propagation,
    PropagationError,
    derive,
    derive_all,
)

__all__ = [
    "BOUNDARY_CLOSE",
    "BOUNDARY_OPEN",
    "DATA_ONLY_INSTRUCTION",
    "PATTERNS",
    "SOURCE_TRUST",
    "TRUST_LEVELS",
    "Assessment",
    "InjectionError",
    "LabelledSegment",
    "Propagation",
    "PropagationError",
    "SuspectedSegment",
    "TrustLevel",
    "assess_all",
    "assess_segment",
    "derive",
    "derive_all",
    "label_for_source",
]
