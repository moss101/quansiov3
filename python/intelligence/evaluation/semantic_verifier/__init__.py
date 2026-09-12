"""Independent semantic verification of a completion claim (RUN-008, DOMAIN.md §4.4).

Canonical owner: `python/intelligence/evaluation/semantic_verifier`. This module supplies the
*intelligence* half of the runtime's `SemanticVerifierPort` seam: the Rust runtime decides
whether a contract needs an independent semantic verdict and what it rejects, and this module
produces that verdict by asking a model that is not the claimant.

It owns no privileged reality. It never commits WorkGraph state, never settles an effect and
never marks work complete: it returns a verdict, and a disagreement is a refusal the runtime
enforces. Independence is a proof obligation, not a hope: when the contract requires an
independent model and the caller cannot name the claimant's model (or names this verifier's own),
the verifier refuses rather than pronounce on its own work.
"""

from __future__ import annotations

from intelligence.evaluation.semantic_verifier.verifier import (
    INDEPENDENCE_UNPROVABLE,
    REQUEST_INVALID,
    VERDICT_KEYS,
    VERDICT_MALFORMED,
    VERIFIER_UNAVAILABLE,
    GatewaySemanticVerifier,
    SemanticVerdict,
    SemanticVerificationError,
    VerificationRequest,
    parse_verdict,
    verifier_prompt,
)

__all__ = [
    "INDEPENDENCE_UNPROVABLE",
    "REQUEST_INVALID",
    "VERDICT_KEYS",
    "VERDICT_MALFORMED",
    "VERIFIER_UNAVAILABLE",
    "GatewaySemanticVerifier",
    "SemanticVerdict",
    "SemanticVerificationError",
    "VerificationRequest",
    "parse_verdict",
    "verifier_prompt",
]
