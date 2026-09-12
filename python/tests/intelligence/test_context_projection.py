"""INT-005 ContextProjection: trust on every segment, a recorded ledger, and bounded packing.

The suites drive the shipped `build_bundle` and `Segment.build` with real segment sets, and check
the acceptance rules directly: a segment without a trust level is refused, the bundle records its
sources/snapshot/policy/ledger, and a stale snapshot is refused rather than served.
"""

from __future__ import annotations

import pathlib

import pytest

from intelligence.context.projection import (
    TRUST_LEVELS,
    ContextError,
    ContextPolicy,
    Segment,
    TrustLevel,
    build_bundle,
)

SNAPSHOT = "snap_canonical_1"


def segment(
    segment_id: str,
    *,
    tokens: int = 10,
    trust: TrustLevel = TrustLevel.UNTRUSTED_EXTERNAL,
    channel: str = "lexical",
    source: str = "artifact://plan",
    score: float = 1.0,
) -> Segment:
    return Segment.build(
        segment_id=segment_id,
        text=f"content of {segment_id}",
        tokens=tokens,
        trust_level=trust,
        source=source,
        snapshot=SNAPSHOT,
        channel=channel,
        score=score,
    )


def test_a_segment_without_a_trust_level_is_rejected() -> None:
    """Acceptance 3: every segment carries a trust_level; one without it is rejected."""
    for missing in (None, "", "trusted_system", 3):
        with pytest.raises(ContextError) as raised:
            Segment.build(
                segment_id="seg_1",
                text="text",
                tokens=1,
                trust_level=missing,
                source="artifact://plan",
                snapshot=SNAPSHOT,
                channel="lexical",
            )
        assert raised.value.code == "VALIDATION_SCHEMA"
        assert raised.value.rule_id == "segment.trust_required"
        assert "no trust level" in raised.value.detail

    # The policy floor is also enforced when packing, not only at construction.
    with pytest.raises(ContextError) as raised:
        build_bundle(
            program_key="program",
            snapshot_id=SNAPSHOT,
            policy=ContextPolicy(token_budget=100, min_trust=TrustLevel.VERIFIED_KNOWLEDGE),
            segments=[segment("seg_untrusted", trust=TrustLevel.UNTRUSTED_EXTERNAL)],
        )
    assert raised.value.rule_id == "segment.trust_required"


def test_the_bundle_records_source_snapshot_policy_and_ledger() -> None:
    """Acceptance 2: a context bundle records source, snapshot, policy and token ledger."""
    policy = ContextPolicy(token_budget=25)
    bundle = build_bundle(
        program_key="program-canonical-key",
        snapshot_id=SNAPSHOT,
        policy=policy,
        segments=[segment("seg_a", tokens=10), segment("seg_b", tokens=20)],
    )
    assert bundle.program_key == "program-canonical-key"
    assert bundle.snapshot_id == SNAPSHOT
    assert bundle.policy is policy
    assert bundle.ledger.budget == 25
    assert bundle.ledger.used == 10, "only the segment that fits is packed"
    assert bundle.ledger.remaining == 15
    assert bundle.ledger.segments == 1
    assert bundle.ledger.dropped_segments == 1
    assert bundle.ledger.dropped_tokens == 20
    assert bundle.ledger.dropped_by_channel == (("lexical", 1),)
    assert bundle.degraded is True
    assert bundle.degradation == ("lexical: 1 segment(s) dropped",)
    # Every kept segment carries its own provenance and trust.
    kept = bundle.segments[0]
    assert kept.source == "artifact://plan" and kept.snapshot == SNAPSHOT
    assert kept.trust_level is TrustLevel.UNTRUSTED_EXTERNAL


def test_packing_is_bounded_and_reports_degradation() -> None:
    """The budget is respected, the best-ranked segments win, and the rest are reported."""
    segments = [
        segment("seg_low", tokens=40, score=0.1),
        segment("seg_high", tokens=40, score=0.9),
        segment("seg_mid", tokens=40, score=0.5),
    ]
    bundle = build_bundle(
        program_key="p",
        snapshot_id=SNAPSHOT,
        policy=ContextPolicy(token_budget=80),
        segments=segments,
    )
    assert [s.segment_id for s in bundle.segments] == ["seg_high", "seg_mid"], "ranked order wins"
    assert bundle.ledger.used == 80
    assert bundle.ledger.dropped_segments == 1
    assert bundle.degraded, "the caller can see that context was dropped"


def test_duplicate_segments_are_deduplicated_and_counted() -> None:
    """Identical content from one source is packed once, and the ledger says so."""
    first = segment("seg_1", source="artifact://plan")
    same_text = Segment.build(
        segment_id="seg_2",
        text=first.text,
        tokens=first.tokens,
        trust_level=TrustLevel.TRUSTED_USER,
        source="artifact://plan",
        snapshot=SNAPSHOT,
        channel="lexical",
        # the same rank, so the trust ladder decides which copy is kept
        score=first.score,
    )
    bundle = build_bundle(
        program_key="p",
        snapshot_id=SNAPSHOT,
        policy=ContextPolicy(token_budget=100),
        segments=[first, same_text],
    )
    assert len(bundle.segments) == 1, "one copy of the same content from the same source"
    assert bundle.segments[0].trust_level is TrustLevel.TRUSTED_USER, "the more trusted copy wins"
    assert bundle.ledger.dropped_segments == 1

    # Deduplication can be turned off, and then both are packed.
    kept_both = build_bundle(
        program_key="p",
        snapshot_id=SNAPSHOT,
        policy=ContextPolicy(token_budget=100, dedupe=False),
        segments=[first, same_text],
    )
    assert len(kept_both.segments) == 2


def test_the_stable_prefix_is_cacheable_and_excludes_volatile_content() -> None:
    """The prefix is a pure function of stable-trust segments, so it can be cached."""
    segments = [
        segment("sys_b", trust=TrustLevel.TRUSTED_SYSTEM, source="system://policy", score=0.1),
        segment("sys_a", trust=TrustLevel.TRUSTED_USER, source="system://profile", score=0.2),
        segment("agent", trust=TrustLevel.AGENT_GENERATED, source="run://prior"),
        segment("tool", trust=TrustLevel.UNTRUSTED_EXTERNAL, source="tool://output"),
    ]
    first = build_bundle(
        program_key="p", snapshot_id=SNAPSHOT, policy=ContextPolicy(token_budget=1_000), segments=segments
    )
    second = build_bundle(
        program_key="p",
        snapshot_id=SNAPSHOT,
        policy=ContextPolicy(token_budget=1_000),
        segments=list(reversed(segments)),
    )
    assert first.render_stable_prefix() == second.render_stable_prefix(), "order does not matter"
    prefix = first.render_stable_prefix()
    assert "system://" in prefix
    assert "run://prior" not in prefix and "tool://output" not in prefix, (
        "agent-generated and external content is not cacheable prefix"
    )
    assert first.render().startswith(prefix), "the stable prefix comes first"


def test_a_stale_snapshot_is_refused() -> None:
    """A projection whose sources have moved is refused, not served."""
    bundle = build_bundle(
        program_key="p",
        snapshot_id=SNAPSHOT,
        policy=ContextPolicy(token_budget=100),
        segments=[segment("seg_1")],
    )
    assert not bundle.is_stale(SNAPSHOT)
    assert bundle.is_stale("snap_canonical_2")
    with pytest.raises(ContextError) as raised:
        build_bundle(
            program_key="p",
            snapshot_id=SNAPSHOT,
            policy=ContextPolicy(token_budget=100),
            segments=[segment("seg_1")],
            current_snapshot="snap_canonical_2",
        )
    assert raised.value.code == "CONFLICT_STATE"
    assert raised.value.rule_id == "projection.snapshot"


def test_trust_levels_match_the_generated_contract() -> None:
    """The ladder is DOMAIN §12's, cross-checked against the generated contract stub."""
    stub = pathlib.Path("intelligence/contracts/generated/quansio/v1/trust/trust_pb2.pyi").read_text()
    for level in TrustLevel:
        assert level.value in TRUST_LEVELS
        assert level.contract_name in stub, f"{level.contract_name} must exist in the contract"
    assert not hasattr(TrustLevel, "UNSPECIFIED"), "an unspecified level is never a segment label"
