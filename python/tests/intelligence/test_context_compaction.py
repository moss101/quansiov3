"""The bounded conversation projection (INT-008's Python half).

Two properties are asserted here, and both are why this module exists rather than a "summarise the old
turns" helper. **A summary carries narrative, never protocol truth** — the section vocabulary is closed
and the protocol surfaces are refused *by name*, with the reason, so a run that obeyed a summary instead
of its protocol state is impossible to construct. And **a projection never presents abandoned history**:
the staleness rule the Rust half applies when an epoch is installed is applied here when one is used, so
a summary covering beyond the caller's committed position is dropped with the reason recorded, and a
summary that cannot fit the budget is dropped whole rather than truncated.
"""

from __future__ import annotations

import pytest

from intelligence.context.compaction import (
    FORBIDDEN_SECTIONS,
    BoundedProjection,
    CompactionSummary,
    SummarySection,
    SummarySectionKind,
    project_conversation,
)
from intelligence.context.projection import ContextError, ContextPolicy, Segment, TrustLevel


def section(
    kind: object = SummarySectionKind.NARRATIVE, text: str = "The run read the runbook.", tokens: int = 10
):
    return SummarySection.build(kind=kind, text=text, tokens=tokens)


def summary(
    *,
    sections: tuple[SummarySection, ...] | None = None,
    to_sequence: int = 40,
    from_sequence: int = 10,
) -> CompactionSummary:
    return CompactionSummary(
        summary_id="cep_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        thread_id="thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        source_from_sequence=from_sequence,
        source_to_sequence=to_sequence,
        sections=sections if sections is not None else (section(),),
        model_route_id="anthropic-sonnet",
    )


def segment(segment_id: str = "seg-1", tokens: int = 10, channel: str = "recent_turn") -> Segment:
    return Segment.build(
        segment_id=segment_id,
        text=f"turn {segment_id}",
        tokens=tokens,
        trust_level=TrustLevel.TRUSTED_USER,
        source="thread",
        snapshot="snap-1",
        channel=channel,
    )


# ------------------------------------------------------- a summary carries narrative only


def test_the_section_vocabulary_is_closed_to_narrative() -> None:
    assert {kind.value for kind in SummarySectionKind} == {
        "narrative",
        "decision",
        "open_question",
        "evidence_ref",
    }
    for kind in SummarySectionKind:
        assert section(kind).kind is kind


def test_protocol_truth_is_refused_by_name_with_the_reason() -> None:
    """The refusal has to name *why*: a writer needs to know a summary is not where a run is obeyed."""
    for name in FORBIDDEN_SECTIONS:
        with pytest.raises(ContextError) as refusal:
            section(kind=name)
        assert refusal.value.rule_id == "summary.section_kind", name
        assert "protocol truth" in refusal.value.detail
        assert "replay" in refusal.value.detail
    with pytest.raises(ContextError) as unknown:
        section(kind="favourite_colour")
    assert unknown.value.rule_id == "summary.section_kind"
    assert "is not one of" in unknown.value.detail


def test_the_forbidden_names_are_the_protocol_surfaces() -> None:
    """A structural check: the list this module refuses covers the state a run obeys."""
    for surface in ("pending_tool_call", "wait", "protocol_state", "next_action", "effect_ledger"):
        assert surface in FORBIDDEN_SECTIONS
    assert not (set(FORBIDDEN_SECTIONS) & {kind.value for kind in SummarySectionKind})


def test_a_section_and_a_summary_are_shape_checked() -> None:
    with pytest.raises(ContextError) as empty_text:
        section(text="   ")
    assert empty_text.value.rule_id == "summary.section_shape"
    with pytest.raises(ContextError) as bad_tokens:
        section(tokens=0)
    assert bad_tokens.value.rule_id == "summary.section_shape"

    with pytest.raises(ContextError) as no_sections:
        summary(sections=())
    assert no_sections.value.rule_id == "summary.shape"
    with pytest.raises(ContextError) as unordered:
        summary(from_sequence=40, to_sequence=10)
    assert unordered.value.rule_id == "summary.range"


def test_the_summary_renders_its_sections_in_order_with_their_kinds() -> None:
    built = summary(
        sections=(
            section(SummarySectionKind.NARRATIVE, "Read the runbook.", 10),
            section(SummarySectionKind.DECISION, "Retention is ninety days.", 12),
            section(SummarySectionKind.EVIDENCE_REF, "evidence://run/1", 4),
        )
    )
    assert built.tokens == 26
    assert built.narrative().splitlines() == [
        "[narrative] Read the runbook.",
        "[decision] Retention is ninety days.",
        "[evidence_ref] evidence://run/1",
    ]


# ------------------------------------------------- the projection never shows abandoned history


def test_a_summary_that_still_covers_the_position_is_used() -> None:
    built = summary(to_sequence=40)
    projection = project_conversation(
        summary=built, segments=(segment(),), position=40, policy=ContextPolicy(token_budget=100)
    )
    assert projection.summary is built
    assert projection.dropped_summary_reason == ""
    assert projection.ledger.used == built.tokens + 10
    assert projection.items == 2


def test_a_summary_covering_beyond_the_position_is_not_presented() -> None:
    """A revert moved the position back: the summary describes history the lineage does not contain."""
    built = summary(to_sequence=40)
    projection = project_conversation(
        summary=built, segments=(segment(),), position=25, policy=ContextPolicy(token_budget=100)
    )
    assert projection.summary is None
    assert "abandoned history is not presented" in projection.dropped_summary_reason
    assert "40" in projection.dropped_summary_reason and "25" in projection.dropped_summary_reason
    assert projection.ledger.used == 10, "the segments are still projected"


def test_a_summary_too_large_for_the_budget_is_dropped_whole() -> None:
    built = summary(sections=(section(text="A long narrative.", tokens=500),))
    projection = project_conversation(
        summary=built, segments=(segment(),), position=40, policy=ContextPolicy(token_budget=100)
    )
    assert projection.summary is None
    assert "dropped whole rather than truncated" in projection.dropped_summary_reason
    assert projection.ledger.used == 10


def test_the_synchronous_fallback_is_a_projection_without_the_summary() -> None:
    """When an epoch cannot be installed, the caller proceeds synchronously rather than blocking.

    Rejecting a stale epoch is only half of INT-008's second build item: the other half is that the run
    carries on. The fallback is a projection with no summary — complete, bounded, and with the reason the
    summary is absent recorded — so a compaction that may not be installed costs the run context, never
    progress.
    """
    stale = summary(to_sequence=40)
    projection = project_conversation(
        summary=stale,
        segments=(segment("a", 20), segment("b", 20)),
        position=25,
        policy=ContextPolicy(token_budget=100),
    )
    assert projection.summary is None, "the stale summary is not presented"
    assert [item.segment_id for item in projection.segments] == ["a", "b"], (
        "the run still gets its recent turns"
    )
    assert projection.ledger.used == 40
    assert projection.dropped_summary_reason, "the fallback is recorded rather than silent"


def test_no_summary_is_a_normal_projection() -> None:
    projection = project_conversation(
        summary=None, segments=(segment(),), position=5, policy=ContextPolicy(token_budget=100)
    )
    assert projection.summary is None
    assert projection.dropped_summary_reason == ""
    assert projection.items == 1


# --------------------------------------------------------------------- the token bound


def test_the_budget_bounds_the_projection_and_accounts_for_what_it_left_out() -> None:
    projection = project_conversation(
        summary=summary(sections=(section(tokens=20),)),
        segments=(
            segment("a", 30),
            segment("b", 30, channel="search_result"),
            segment("c", 30, channel="search_result"),
        ),
        position=40,
        policy=ContextPolicy(token_budget=80),
    )
    assert [item.segment_id for item in projection.segments] == ["a", "b"]
    assert projection.ledger == projection.ledger.__class__(
        budget=80,
        used=20 + 60,
        dropped_tokens=30,
        segments=2,
        dropped_segments=1,
        dropped_by_channel=(("search_result", 1),),
    )
    assert projection.ledger.remaining == 0


def test_a_projection_cannot_exceed_the_budget() -> None:
    for budget in (1, 10, 45, 200):
        projection = project_conversation(
            summary=summary(sections=(section(tokens=10),)),
            segments=tuple(segment(f"seg-{index}", 7) for index in range(10)),
            position=40,
            policy=ContextPolicy(token_budget=budget),
        )
        assert projection.ledger.used <= budget


def test_a_projection_needs_a_committed_position() -> None:
    with pytest.raises(ContextError) as refusal:
        project_conversation(summary=None, segments=(), position=-1, policy=ContextPolicy(token_budget=10))
    assert refusal.value.rule_id == "projection.shape"


def test_the_projection_has_nowhere_to_put_protocol_state() -> None:
    """The structural half of 'tool/protocol state stays outside summaries'."""
    import dataclasses

    fields = {field.name for field in dataclasses.fields(BoundedProjection)}
    assert fields == {"summary", "segments", "ledger", "dropped_summary_reason"}, (
        "a projection carries the summary, its segments and the ledger: there is no field a tool call, "
        "a wait or a protocol state could be smuggled through"
    )
    assert isinstance(
        project_conversation(summary=None, segments=(), position=0, policy=ContextPolicy(token_budget=10)),
        BoundedProjection,
    )
