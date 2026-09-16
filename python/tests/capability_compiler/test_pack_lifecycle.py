"""CAP-005: qualification gating, promotion, resolution and deterministic rollback."""

from __future__ import annotations

import pytest

from intelligence.capability_compiler import IngestionSource, compile_pack
from intelligence.capability_compiler.lifecycle import (
    LifecycleError,
    PackLifecycle,
    Qualification,
    resolve,
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0CAP005"

SOP = (
    "The deploy tool is an endpoint the pack uses. Step 1: run the migration. "
    "Then verify that the health endpoint responds. "
    "Data retention must not exceed ninety days."
)


def draft() -> object:
    return compile_pack(
        "onboarding",
        TENANT,
        [IngestionSource("src_sop", TENANT, "sop", SOP)],
    )


PASSING = Qualification(passed=("nav", "budget"), failed=(), protected_failed=())
FAILING = Qualification(passed=(), failed=("nav",), protected_failed=())
PROTECTED = Qualification(passed=("nav",), failed=(), protected_failed=("safety",))


def test_qualification_gates_promotion() -> None:
    """A failing or protected-failing evaluation never promotes."""
    d = draft()
    with pytest.raises(LifecycleError, match="case\\(s\\) failed"):
        qualify = None  # placeholder to keep the name meaningful
        del qualify
        from intelligence.capability_compiler.lifecycle import qualify

        qualify(d, FAILING)  # type: ignore[arg-type]
    with pytest.raises(LifecycleError, match="protected case"):
        from intelligence.capability_compiler.lifecycle import qualify

        qualify(d, PROTECTED)  # type: ignore[arg-type]
    with pytest.raises(LifecycleError):
        from intelligence.capability_compiler.lifecycle import qualify

        class NotCandidate:
            status = "published"

        qualify(NotCandidate(), PASSING)  # type: ignore[arg-type]


def test_promotion_resolves_into_existing_primitives_without_widening() -> None:
    from intelligence.capability_compiler.lifecycle import qualify

    d = draft()
    digest = qualify(d, PASSING)  # type: ignore[arg-type]
    resolution = resolve(
        d,  # type: ignore[arg-type]
        digest,
        caller_tools=frozenset({"deploytool"}),
        caller_capabilities=frozenset({"deploy"}),
    )
    assert resolution["workgraph_template"]["branches"], "process steps become branches"
    assert resolution["widened"] is False
    # Resolution intersects: the caller's own authority bounds the pack.
    assert set(resolution["tools"]) <= {"deploytool"}


def test_lifecycle_promotes_versions_and_rolls_back_deterministically() -> None:

    lifecycle = PackLifecycle(name="onboarding", tenant_id=TENANT)
    with pytest.raises(LifecycleError):
        lifecycle.current()

    first = lifecycle.promote(draft(), PASSING)  # type: ignore[arg-type]
    assert first.version == 1
    second = lifecycle.promote(draft(), PASSING)  # type: ignore[arg-type]
    assert second.version == 2
    assert lifecycle.current().version == 2

    rolled = lifecycle.rollback()
    assert rolled.version == 1
    with pytest.raises(LifecycleError, match="no prior published"):
        lifecycle.rollback()
    assert lifecycle.current().version == 1
    as_mapping = lifecycle.current().as_mapping()
    assert as_mapping["content_digest"] == first.content_digest
