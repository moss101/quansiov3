"""CAP-006: WikiSkill eval campaign, budget regression, and disable/fallback.

The suite loads the shipped pack at `packs/skills/wiki`, drives SearchProgram +
Knowledge Fabric + ContextProjection (no new subsystem), and checks the two
acceptance rules: the skill passes pinned thresholds, and disabling it leaves
core search/context functional.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType

from intelligence.context.projection import ContextPolicy, Segment, TrustLevel, build_bundle
from intelligence.context.search import SearchProgram
from intelligence.knowledge.models import KnowledgeStatus
from intelligence.skills import CapabilitySnapshot, SkillVersionRef, resolve

ROOT = Path(__file__).resolve().parents[3]
WIKI_NAVIGATE = ROOT / "packs" / "skills" / "wiki" / "navigate.py"


def load_wiki() -> ModuleType:
    spec = importlib.util.spec_from_file_location("wiki_skill_navigate", WIKI_NAVIGATE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def wiki_version(*, status: str = "active") -> SkillVersionRef:
    wiki = load_wiki()
    manifest = wiki.load_skill_manifest()
    return SkillVersionRef.build(
        skill_id="skl_wiki",
        version_id="sklv_wiki_1",
        skill_name="wiki",
        semver="1.0.0",
        status=status,
        manifest={
            "instructions": manifest.instructions,
            "examples": list(manifest.examples),
            "tool_needs": list(manifest.tool_needs),
            "capability_needs": list(manifest.capability_needs),
            "eval_suite_id": manifest.eval_suite_id,
            "compatibility": list(manifest.compatibility),
            "recovery_guidance": manifest.recovery_guidance,
        },
    )


def test_skill_eval_campaign_passes_pinned_thresholds() -> None:
    """Named test: skill eval campaign. Navigation, provenance, links and budget hold."""
    wiki = load_wiki()
    pages = wiki.load_corpus()
    assert pages["kn_wiki_retired"].entry.status is KnowledgeStatus.DELETED
    assert not pages["kn_wiki_retired"].retrievable
    _run, verdict = wiki.run_campaign(pages=pages)
    assert verdict.passed, verdict.reason
    assert not verdict.promotion_blocked
    by_metric = {item.metric: item for item in verdict.verdicts}
    assert by_metric["navigation_recall"].passed
    assert by_metric["provenance_preserved"].passed
    assert by_metric["link_preservation"].passed
    assert by_metric["budget_respected"].passed

    onboarding = wiki.navigate("onboarding a teammate", pages=pages, expected_ids=("kn_wiki_onboarding",))
    assert onboarding.hit_ids == ("kn_wiki_onboarding",)
    assert onboarding.hits[0].provenance_intact()
    assert "kn_wiki_retired" not in onboarding.hit_ids
    artifacts = wiki.navigate("versioned source files", pages=pages)
    assert artifacts.hit_ids[0] == "kn_wiki_artifacts"
    assert artifacts.hits[0].links == pages["kn_wiki_artifacts"].links


def test_budget_regression_records_dropped_segments() -> None:
    """Named test: budget regression. Packing respects the token budget and reports drops."""
    wiki = load_wiki()
    pages = wiki.load_corpus()
    result = wiki.navigate("knowledge space", pages=pages, budget=12)
    assert result.budget_ok()
    assert result.bundle.ledger.used <= 12
    assert result.bundle.degraded
    assert result.bundle.ledger.dropped_segments > 0
    assert "kn_wiki_retired" not in result.hit_ids
    assert set(result.hit_ids) <= {
        "kn_wiki_onboarding",
        "kn_wiki_artifacts",
        "kn_wiki_policy",
        "kn_wiki_search",
        "kn_wiki_teammates",
    }
    packed_ids = {segment.segment_id.removeprefix("seg_") for segment in result.bundle.segments}
    assert packed_ids <= set(result.hit_ids)
    assert result.bundle.ledger.used == sum(segment.tokens for segment in result.bundle.segments)


def test_disable_fallback_leaves_core_search_and_context_functional() -> None:
    """Named test: disable/fallback. Without WikiSkill, SearchProgram and context still work."""
    snapshot = CapabilitySnapshot(
        tool_names=frozenset({"knowledge.cite"}),
        capability_needs=frozenset({"read.internal"}),
    )
    enabled = resolve(
        task_text="navigate the wiki for the source page",
        snapshot=snapshot,
        candidates=[wiki_version(status="active")],
    )
    assert enabled.resolved_ids() == ("sklv_wiki_1",)

    disabled = resolve(
        task_text="navigate the wiki for the source page",
        snapshot=snapshot,
        candidates=[wiki_version(status="deprecated")],
    )
    assert disabled.resolved == ()
    assert any(item.rule_id == "skill.not_active" for item in disabled.excluded)

    missing_tool = resolve(
        task_text="navigate the wiki",
        snapshot=CapabilitySnapshot(
            tool_names=frozenset({"fs.read"}),
            capability_needs=frozenset({"read.internal"}),
        ),
        candidates=[wiki_version(status="active")],
    )
    assert missing_tool.resolved == ()

    program = SearchProgram.parse(
        {
            "channels": [{"channel": "lexical", "text": "onboarding"}],
            "predicates": [{"field": "kind", "operator": "eq", "value": "wiki_page"}],
            "limit": 5,
        }
    )
    assert program.channels[0].value == "onboarding"
    bundle = build_bundle(
        program_key=program.canonical_json(),
        snapshot_id="core_without_wiki",
        policy=ContextPolicy(token_budget=32, min_trust=TrustLevel.VERIFIED_KNOWLEDGE),
        segments=[
            Segment.build(
                segment_id="seg_core",
                text="Onboarding still works when WikiSkill is disabled.",
                tokens=8,
                trust_level=TrustLevel.VERIFIED_KNOWLEDGE,
                source="art_wiki_onboarding",
                snapshot="core_without_wiki",
                channel="lexical",
                score=1.0,
            )
        ],
        current_snapshot="core_without_wiki",
    )
    assert bundle.ledger.used == 8
    assert not bundle.degraded
    assert "Onboarding still works" in bundle.render()
