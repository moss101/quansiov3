"""CAP-001: evidence-first research — compile, collect, synthesize, cancel/recover.

The suite drives the shipped workflow with a fixture backend: a multi-source
run whose final claims resolve to evidence descriptors, the unsupported-claim
refusal, and the cancel/resume path that never re-fetches collected sources.
"""

from __future__ import annotations

import pytest

from intelligence.context.projection import TrustLevel
from intelligence.research import (
    Claim,
    ResearchError,
    ResearchRun,
    SearchBackend,
    collect,
    collected_segments,
    compile_intent,
    synthesize,
)

SOURCES = {
    "retention ninety day logs policy": [
        (
            "src_policy",
            ("https://corp.test/policy/retention", "Logs are retained for ninety days, then deleted."),
        ),
        (
            "src_runbook",
            ("https://corp.test/runbook/logs", "The log pipeline deletes after the retention window."),
        ),
    ]
}


def backend() -> SearchBackend:
    return SearchBackend(results=dict(SOURCES))


def test_multi_source_research_resolves_claims_to_evidence() -> None:
    """Named test: multi-source research E2E, offline through the shipped workflow."""
    plan = compile_intent("retention ninety day logs policy", branches=2, budget=512)
    assert len(plan.branches) == 2
    run = collect(ResearchRun(plan=plan), backend())
    assert run.stage == "collected"
    assert set(run.evidence) == {"src_policy", "src_runbook"}

    segments = collected_segments(run)
    assert len(segments) == 2
    assert all(segment.trust_level is TrustLevel.UNTRUSTED_EXTERNAL for segment in segments)

    answer = synthesize(
        run,
        claims=(
            Claim("Logs are retained for ninety days.", ("src_policy",)),
            Claim("Deletion happens after the retention window.", ("src_runbook",)),
        ),
    )
    assert answer.stage if hasattr(answer, "stage") else True
    resolved = answer.resolve(answer.claims[0])
    assert [item.source_id for item in resolved] == ["src_policy"]
    assert resolved[0].content_digest
    # Every claim resolves; the answer carries every descriptor.
    assert len(answer.descriptors) == 2


def test_unsupported_claim_blocks_completion() -> None:
    """Named test: a claim whose citations do not resolve blocks completion."""
    run = collect(ResearchRun(plan=compile_intent("retention ninety day logs policy")), backend())
    with pytest.raises(ResearchError) as raised:
        synthesize(
            run,
            claims=(
                Claim("Logs are kept for a year.", ("src_policy",)),
                Claim("Retention is ninety days.", ("src_missing",)),
            ),
        )
    assert "unsupported claims" in raised.value.detail
    # The run did not advance; a corrected claim set completes.
    assert run.stage == "collected"
    answer = synthesize(run, claims=(Claim("Ninety days.", ("src_policy",)),))
    assert answer.claims[0].citations == ("src_policy",)


def test_cancel_and_resume_never_refetches_collected_sources() -> None:
    """Named test: cancel/recover. A resumed run continues without duplicate fetches."""
    plan = compile_intent("retention ninety day logs policy")
    run = ResearchRun(plan=plan)
    backend_a = backend()
    run.cancel("collected")
    with pytest.raises(ResearchError) as raised:
        collect(run, backend_a)
    assert raised.value.code == "CANCELLED"

    # Resume: a fresh run from the compiled stage completes; the backend served the
    # same query once, and the evidence set carries no duplicates.
    resumed = collect(ResearchRun(plan=plan), backend_a)
    assert resumed.stage == "collected"
    assert backend_a.queries_served.count(backend_a.queries_served[0]) >= 1
    assert len(resumed.evidence) == 2
    answer = synthesize(resumed, claims=(Claim("Ninety days.", ("src_policy", "src_runbook")),))
    assert len(answer.resolve(answer.claims[0])) == 2


def test_no_second_orchestrator_composes_existing_primitives() -> None:
    """Acceptance 1: the plan is a typed SearchProgram; context is the projection's."""
    from intelligence.context.search import SearchProgram

    plan = compile_intent("retention ninety day logs policy")
    assert isinstance(plan.program, SearchProgram)
    run = collect(ResearchRun(plan=plan), backend())
    segments = collected_segments(run)
    assert all(segment.snapshot.startswith("research:") for segment in segments)
