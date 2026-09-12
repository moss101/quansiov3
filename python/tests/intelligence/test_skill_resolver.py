"""INT-009 skill resolution: production state, capability narrowing, and a bounded deterministic result.

The suites drive the shipped `resolve` and `SkillVersionRef.build` with real candidate sets and
snapshots, and check both acceptance rules directly: an unapproved version cannot enter production
context, and resolution is bounded and deterministic for fixed inputs.
"""

from __future__ import annotations

import pytest

from intelligence.skills import (
    SKILL_STATES,
    CapabilitySnapshot,
    SkillError,
    SkillManifest,
    SkillStatus,
    SkillVersionRef,
    resolve,
)

SNAPSHOT = CapabilitySnapshot(
    tool_names=frozenset({"fs.read", "terminal.exec"}),
    capability_needs=frozenset({"read.internal", "process.exec.sandboxed"}),
)


def version(
    version_id: str,
    *,
    status: str = "active",
    skill_name: str = "review-pr",
    semver: str = "1.0.0",
    tool_needs: tuple[str, ...] = ("fs.read",),
    capability_needs: tuple[str, ...] = ("read.internal",),
    examples: tuple[str, ...] = ("review the pr",),
) -> SkillVersionRef:
    return SkillVersionRef.build(
        skill_id=f"skl_{skill_name}",
        version_id=version_id,
        skill_name=skill_name,
        semver=semver,
        status=status,
        manifest={
            "instructions": "read the diff, then comment",
            "examples": list(examples),
            "tool_needs": list(tool_needs),
            "capability_needs": list(capability_needs),
            "eval_suite_id": "eval_1",
        },
        # provenance is a version field (§11.5), not a manifest key — the manifest vocabulary
        # refuses it, which is the fail-closed behaviour the last test asserts.
        provenance="pack://review",
    )


def test_only_active_versions_resolve() -> None:
    """Acceptance 1: an unapproved (or not-yet-production) version cannot enter context."""
    candidates = [version(f"sklv_{state}", status=state) for state in SKILL_STATES]
    resolution = resolve(
        task_text="please review the pr",
        snapshot=SNAPSHOT,
        candidates=candidates,
        max_skills=10,
    )
    assert [v.status for v in resolution.resolved] == [SkillStatus.ACTIVE], "only the active version resolves"
    not_active = [e for e in resolution.excluded if e.rule_id == "skill.not_active"]
    assert len(not_active) == len(SKILL_STATES) - 1
    approved = [e for e in not_active if "approved" in e.detail]
    assert approved, "an approved-but-not-active version is refused exactly like a draft"
    assert all(e.rule_id == "skill.not_active" for e in approved)
    # Every candidate is accounted for exactly once.
    seen = {v.version_id for v in resolution.resolved} | {e.version_id for e in resolution.excluded}
    assert seen == {v.version_id for v in candidates}


def test_a_skill_cannot_widen_the_callers_authority() -> None:
    """Acceptance 1's companion: resolution narrows — it never grants what the caller lacks."""
    within = version("sklv_ok", tool_needs=("fs.read",), capability_needs=("read.internal",))
    needs_missing_tool = version("sklv_tool", tool_needs=("browser.click",))
    needs_missing_capability = version("sklv_cap", capability_needs=("fs.write.host",))
    resolution = resolve(
        task_text="please review the pr",
        snapshot=SNAPSHOT,
        candidates=[within, needs_missing_tool, needs_missing_capability],
        max_skills=10,
    )
    assert resolution.resolved_ids() == ("sklv_ok",)
    rules = {e.rule_id for e in resolution.excluded}
    assert rules == {"skill.tool_not_available", "skill.capability_not_granted"}
    # The snapshot the result runs under is the caller's own, unchanged.
    assert resolution.snapshot is SNAPSHOT
    assert resolution.snapshot.tool_names == SNAPSHOT.tool_names
    assert resolution.snapshot.capability_needs == SNAPSHOT.capability_needs


def test_resolution_is_bounded_and_deterministic() -> None:
    """Acceptance 2: bounded by the resolver budget, and a pure function of the inputs."""
    candidates = [
        version("sklv_a", skill_name="review-pr", semver="1.0.0"),
        version("sklv_b", skill_name="review-pr", semver="2.0.0"),
        version("sklv_c", skill_name="review-pr", semver="0.9.0"),
        version("sklv_d", skill_name="review-pr", semver="3.0.0"),
    ]
    first = resolve(task_text="please review the pr", snapshot=SNAPSHOT, candidates=candidates, max_skills=2)
    assert first.budget == 2
    assert len(first.resolved) == 2, "the budget bounds resolution"
    assert first.budget_used == 2
    assert "resolver.budget" in first.rules_fired(), "the dropped candidates say why"

    # Same inputs, shuffled candidate order: identical resolution.
    second = resolve(
        task_text="please review the pr",
        snapshot=SNAPSHOT,
        candidates=list(reversed(candidates)),
        max_skills=2,
    )
    assert second.resolved_ids() == first.resolved_ids()
    assert [e.version_id for e in second.excluded] == [e.version_id for e in first.excluded]

    # A different task text resolves nothing relevant, and nothing is invented.
    empty = resolve(task_text="order lunch", snapshot=SNAPSHOT, candidates=candidates, max_skills=2)
    assert empty.resolved == ()
    assert empty.rules_fired() == ("skill.irrelevant",)

    # Relevance beats a larger semver: the name/example match decides the order.
    unrelated = version("sklv_z", skill_name="file-report", semver="9.9.9", examples=("write the report",))
    ranked = resolve(
        task_text="review the pr",
        snapshot=SNAPSHOT,
        candidates=[unrelated, version("sklv_match", skill_name="review-pr", semver="1.0.0")],
        max_skills=1,
    )
    assert ranked.resolved_ids() == ("sklv_match",)


def test_a_manifest_with_unknown_keys_or_shapes_is_refused() -> None:
    """A skill cannot smuggle behaviour through a field the resolver does not understand."""
    with pytest.raises(SkillError) as raised:
        SkillManifest.parse({"instructions": "x", "grant_permissions": ["fs.write.host"]})
    assert raised.value.rule_id == "skill.manifest_keys"
    assert raised.value.code == "VALIDATION_SCHEMA"

    with pytest.raises(SkillError) as raised:
        SkillManifest.parse({"tool_needs": "fs.read"})
    assert raised.value.rule_id == "skill.manifest_shape"

    with pytest.raises(SkillError) as raised:
        SkillVersionRef.build(
            skill_id="skl_1",
            version_id="sklv_1",
            skill_name="n",
            semver="1.0.0",
            status="production",
        )
    assert raised.value.rule_id == "skill.status"


def test_the_state_ladder_matches_domain_11_5() -> None:
    """The states are §11.5's, and only one of them resolves."""
    assert SKILL_STATES == (
        "draft",
        "candidate",
        "evaluating",
        "approved",
        "active",
        "deprecated",
        "retired",
        "rejected",
    )
    resolving = [state for state in SkillStatus if state.resolves]
    assert resolving == [SkillStatus.ACTIVE]
    assert SkillStatus.REJECTED.previous == (SkillStatus.EVALUATING,)
    assert SkillStatus.ACTIVE.previous == (SkillStatus.APPROVED,)
