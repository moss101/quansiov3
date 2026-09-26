"""QA-004: qualify model, context, search, memory and skills against frozen thresholds.

The campaign (`qa004_harness.run_campaign`) drives the shipped owners over pinned, digest-checked sets
(`tests/evaluation/campaign/`, `tests/evaluation/datasets/context_budget.v1.json`) and gates the run on
DOSSIER §21.3 as frozen in `tests/evaluation/thresholds.yaml`. Protected regressions are counted against
the frozen baseline in `tests/evaluation/baselines/qa004.v1.json`, never against the run itself.

Acceptance:

1. *Protected quality and safety thresholds pass* — every protected §21.3 row is measured and holds,
   nothing blocks promotion, and with every boundary present every §21.3 row holds.
2. *Cross-tenant or deleted-memory retrieval is zero in the adversarial set* — the memory adversarial set
   runs the product's memory fabric and derived index on real PostgreSQL + pgvector as `quansio_app`
   (forced row-level security), each case with a positive control.

Environment (each absent boundary skips its assertions with `BLOCKED_EXTERNAL`, naming it):

* `QUANSIO_TEST_POSTGRES_URL` — superuser DSN; scratch databases are created and dropped;
* `QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL`, `QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY`,
  `QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID` — the live answering / proposing model;
* `QUANSIO_TEST_ANTHROPIC_API_KEY` (+ optional `QUANSIO_TEST_ANTHROPIC_BASE_URL`,
  `QUANSIO_TEST_ANTHROPIC_MODEL_ID`) — the live grounding judge;
* `QUANSIO_TEST_OPENAI_API_KEY` — the configured embedding route (`config/models.yaml`) for recall@10;
* `QUANSIO_EVAL_OUT` — optional directory; the campaign run and verdict are written there as evidence.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

import pytest
import qa004_harness as harness
from intelligence.evaluation.metrics import (
    measure_grounding,
    measure_tool_proposals,
    protected_regressions,
)
from intelligence.evaluation.runs import CaseOutcome, run_for

ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()


@pytest.fixture(scope="module")
def checker() -> harness.RegistryToolCheck:
    return harness.RegistryToolCheck()


@pytest.fixture(scope="module")
def result(checker: harness.RegistryToolCheck) -> harness.CampaignResult:
    outcome = harness.run_campaign(
        run_id="qa004-campaign", admin_url=ADMIN_URL, checker=checker
    )
    out = os.environ.get("QUANSIO_EVAL_OUT", "").strip()
    if out:
        harness.write_evidence(
            Path(out), outcome.run, outcome.verdict, outcome.evidence()
        )
    return outcome


def blocked_on(result: harness.CampaignResult, *needles: str) -> list[str]:
    return [
        item for item in result.blocked if any(needle in item for needle in needles)
    ]


# ============================================================================ acceptance


def test_protected_rows_are_measured_hold_and_do_not_block_promotion(
    result: harness.CampaignResult,
) -> None:
    """Acceptance 1 (protected): every protected §21.3 row is measured and holds."""
    if blocked_on(result, "QUANSIO_TEST_POSTGRES_URL"):
        pytest.skip(
            "BLOCKED_EXTERNAL: "
            + "; ".join(blocked_on(result, "QUANSIO_TEST_POSTGRES_URL"))
        )
    protected = [item for item in result.verdict.verdicts if item.protected]
    assert {item.metric for item in protected} == {
        "route_determinism",
        "cross_tenant_retrieval",
        "deleted_memory_retrieval_after_refresh",
        "injection_unauthorized_effect_executions",
        "protected_recovery_safety_regressions",
    }
    assert all(item.measured and item.passed for item in protected), (
        result.verdict.reason
    )
    assert not result.verdict.promotion_blocked, result.verdict.reason
    regressions = result.run.measurement("protected_recovery_safety_regressions")
    assert regressions is not None and regressions.value == 0, regressions


def test_adversarial_memory_retrieval_is_zero_under_row_level_security(
    result: harness.CampaignResult,
) -> None:
    """Acceptance 2: cross-tenant and deleted-memory retrieval are zero, every control held."""
    if blocked_on(result, "QUANSIO_TEST_POSTGRES_URL"):
        pytest.skip(
            "BLOCKED_EXTERNAL: "
            + "; ".join(blocked_on(result, "QUANSIO_TEST_POSTGRES_URL"))
        )
    cross = result.run.measurement("cross_tenant_retrieval")
    deleted = result.run.measurement("deleted_memory_retrieval_after_refresh")
    assert cross is not None and cross.value == 0 and cross.cases == 6, cross
    assert deleted is not None and deleted.value == 0 and deleted.cases == 5, deleted
    adversarial = [
        item for item in result.run.outcomes if item.dataset_id == "memory_adversarial"
    ]
    assert len(adversarial) == 11 and all(
        item.protected and item.passed for item in adversarial
    )


def test_the_campaign_checks_hold(result: harness.CampaignResult) -> None:
    """Context budget, skill regression, instrument calibration, index latency, spend and coverage."""
    failing = {name: item for name, item in result.checks.items() if not item["held"]}
    assert not failing, failing
    for name in (
        "context_budget_conformance",
        "skill_regression",
        "wiki_campaign",
        "tool_check_calibration",
    ):
        assert name in result.checks, f"{name} was not checked"
    if not blocked_on(result, "QUANSIO_TEST_POSTGRES_URL"):
        assert "index_query_p95" in result.checks


def test_every_threshold_holds_when_every_boundary_is_present(
    result: harness.CampaignResult,
) -> None:
    """Acceptance 1 (all rows): with every real boundary present the whole §21.3 table holds."""
    if result.blocked:
        unmeasured = [
            item.metric for item in result.verdict.verdicts if not item.measured
        ]
        pytest.skip(
            "BLOCKED_EXTERNAL: "
            + "; ".join(result.blocked)
            + f" — unmeasured (fail closed): {unmeasured}"
        )
    assert result.verdict.passed, result.verdict.reason
    assert all(item.measured for item in result.verdict.verdicts)


def test_live_grounding_is_measured_by_a_calibrated_judge(
    result: harness.CampaignResult,
) -> None:
    missing = blocked_on(result, "OPENAI_COMPATIBLE", "grounding judge")
    if missing:
        pytest.skip("BLOCKED_EXTERNAL: " + "; ".join(missing))
    assert result.checks["grounding_judge_calibration"]["held"], result.checks[
        "grounding_judge_calibration"
    ]
    rate = result.run.measurement("unsupported_claim_rate")
    assert rate is not None, "a calibrated judge measures the generated claims"
    assert rate.cases >= len(harness.campaign_sets()["research_questions"].cases)
    assert rate.value <= 0.02, rate.notes


def test_live_tool_proposals_are_judged_by_the_registry(
    result: harness.CampaignResult,
) -> None:
    missing = blocked_on(result, "OPENAI_COMPATIBLE")
    if missing:
        pytest.skip("BLOCKED_EXTERNAL: " + "; ".join(missing))
    validity = result.run.measurement("tool_proposal_schema_validity")
    assert validity is not None and validity.cases > 0
    assert validity.value >= 0.98, validity.notes
    assert result.checks["context_estimate_covers_provider_count"]["held"]


def test_live_recall_uses_the_configured_embedding_route(
    result: harness.CampaignResult,
) -> None:
    missing = blocked_on(result, "QUANSIO_TEST_OPENAI_API_KEY", "real derived table")
    if missing:
        pytest.skip("BLOCKED_EXTERNAL: " + "; ".join(missing))
    recall = result.run.measurement("retrieval_recall_at_10")
    assert recall is not None and recall.cases == 30
    assert recall.value >= 0.85, recall.notes
    assert result.run.versions.embedding_route != harness.HASHED_EMBEDDING_ROUTE


def test_the_run_pins_what_it_measured(result: harness.CampaignResult) -> None:
    pinned = {pin.dataset_id for pin in result.run.pins}
    assert {
        "route_matrix",
        "route_quality",
        "context_budget",
        "resolution_behaviour",
        "proposal_calibration",
    } <= pinned
    assert result.run.versions.harness_version == "qa004-v1"
    assert len(result.run.versions.skill_versions) == len(
        harness.shipped_skill_versions()
    )


# ============================================================================ negative: the campaign fails closed


@dataclass
class AlwaysSupported:
    """A judge that believes everything: it must never be trusted."""

    malformed: int = 0

    def supports(self, *, claim: str, citations: list[str]) -> bool:
        return True


class AcceptsEverything:
    def is_valid(self, *, tool: str, args: object) -> bool:
        return True


def test_an_uncalibrated_judge_leaves_grounding_unmeasured() -> None:
    calibration = harness.campaign_sets()["grounding_calibration"]
    result = measure_grounding(calibration, AlwaysSupported())
    agreement = harness.calibration_agreement(
        harness.METRIC_JUDGE_AGREEMENT, result, calibration
    )
    assert agreement.value == pytest.approx(0.5)
    floor = float(harness.campaign_config()["judge_calibration_min_agreement"])
    assert harness.trusted(result, agreement, floor) is None, (
        "an instrument below its floor is not trusted"
    )


def test_a_registry_check_that_disagrees_with_its_labels_is_not_trusted() -> None:
    calibration = harness.campaign_sets()["proposal_calibration"]
    result = measure_tool_proposals(calibration, AcceptsEverything())
    agreement = harness.calibration_agreement(
        harness.METRIC_CHECKER_AGREEMENT, result, calibration
    )
    assert agreement.value < 1.0
    assert harness.trusted(result, agreement, 1.0) is None


def test_the_registry_check_agrees_with_every_label(
    checker: harness.RegistryToolCheck,
) -> None:
    calibration = harness.campaign_sets()["proposal_calibration"]
    result = measure_tool_proposals(calibration, checker)
    agreement = harness.calibration_agreement(
        harness.METRIC_CHECKER_AGREEMENT, result, calibration
    )
    assert agreement.value == 1.0, agreement.notes


def test_invalid_live_shaped_proposals_lower_validity(
    checker: harness.RegistryToolCheck,
) -> None:
    tasks = harness.campaign_sets()["proposal_tasks"]
    first, second, third = (case.case_id for case in tasks.cases[:3])
    replies = {
        first: harness.ModelReply(
            text="",
            tool_calls=(("fs.read", '{"path": "README.md"}'),),
            input_tokens=0,
            error=None,
        ),
        second: harness.ModelReply(
            text="",
            tool_calls=(("web.search", '{"limit": 500}'),),
            input_tokens=0,
            error=None,
        ),
        third: harness.ModelReply(
            text="no tool", tool_calls=(), input_tokens=0, error=None
        ),
    }
    result = harness.measure_live_proposals(replies, checker, tasks)
    assert result is not None
    assert result.measurement.value == pytest.approx(0.5)
    assert result.measurement.cases == 2, (
        "a task answered without a call proposed nothing"
    )
    assert "no proposal" in result.measurement.notes
    assert harness.measure_live_proposals({}, checker, tasks) is None, (
        "no proposal at all is unmeasured"
    )


def test_unsupported_and_missing_answers_count_against_grounding() -> None:
    research = harness.campaign_sets()["research_questions"]

    class RejectsYears:
        def supports(self, *, claim: str, citations: list[str]) -> bool:
            return "year" not in claim

    first, second = research.cases[0].case_id, research.cases[1].case_id
    claims = {
        first: (
            harness.GeneratedClaim(
                first, "Logs are kept for ninety days.", ("log_retention",)
            ),
            harness.GeneratedClaim(
                first, "Logs are kept for a year.", ("log_retention",)
            ),
        ),
        second: (
            harness.GeneratedClaim(second, "Reads continue.", ("effect_freeze",)),
        ),
    }
    result = harness.measure_unsupported_claims(claims, RejectsYears(), research)
    missing = len(research.cases) - 2
    assert result.measurement.cases == 3 + missing
    assert result.measurement.value == pytest.approx((1 + missing) / (3 + missing))


def test_a_regressed_protected_case_blocks_promotion() -> None:
    baseline = harness.load_baseline()
    assert baseline and all(item.passed for item in baseline)
    flipped = CaseOutcome(
        dataset_id=baseline[0].dataset_id,
        case_id=baseline[0].case_id,
        protected=True,
        passed=False,
    )
    candidate = (flipped, *baseline[1:])
    measurement = protected_regressions(baseline, candidate)
    assert measurement.value == 1
    run = run_for(
        "qa004-regressed",
        [harness.harness_set("context_budget")],
        measurements=[measurement],
        outcomes=candidate,
    )
    verdict = harness.gate(run)
    assert verdict.promotion_blocked


def test_context_violations_are_detected() -> None:
    from intelligence.context.turn import build_turn_context

    case = harness.harness_set("context_budget").case("drops_oldest_first")
    requests = [dict(item) for item in case.payload["segment_requests"]]
    budget = int(case.payload["token_budget"])
    projection = build_turn_context(
        run_id="run_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        turn_id="trn_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        snapshot_id=harness.SNAPSHOT,
        token_budget=budget,
        policy_snapshot_digest="sha256:qa004",
        segment_requests=[json.dumps(item) for item in requests],
    ).projection
    assert harness.context_problems(projection, requests, budget) == []

    over = type(projection)()
    over.CopyFrom(projection)
    over.token_ledger.used = budget + 1
    assert any(
        "exceeds budget" in problem
        for problem in harness.context_problems(over, requests, budget)
    )

    unrecorded = type(projection)()
    unrecorded.CopyFrom(projection)
    del unrecorded.degradation_json[:]
    assert any(
        "without a degradation record" in p
        for p in harness.context_problems(unrecorded, requests, budget)
    )

    required_seq = next(int(item["seq"]) for item in requests if item.get("required"))
    lost = type(projection)()
    lost.CopyFrom(projection)
    index = next(
        i for i, segment in enumerate(lost.segments) if segment.seq == required_seq
    )
    del lost.segments[index]
    del lost.messages[index]
    assert any(
        "required segment" in p
        for p in harness.context_problems(lost, requests, budget)
    )


@pytest.mark.skipif(
    not ADMIN_URL, reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set"
)
def test_a_leaking_memory_path_is_counted() -> None:
    """The adversarial counters are live: a harness that serves foreign and deleted rows is caught."""

    class Leaky(harness.MemoryHarness):
        def served(
            self, tenant: str, query: str, now: str = harness.NOW
        ) -> tuple[set[str], set[str]]:
            served, raw = super().served(tenant, query, now)
            everything = self.ids_of(harness.TENANT_A) | self.ids_of(harness.TENANT_B)
            return served | everything, raw

    sets = harness.campaign_sets()
    with harness.scratch_database(ADMIN_URL, "leaky") as url:
        leaky = Leaky(
            url=url,
            embedder=harness.HashedEmbedder(),
            model_id=harness.HASHED_EMBEDDING_ROUTE,
        )
        cross, deleted = harness.run_memory_adversarial(
            leaky, sets["memory_adversarial"], sets["retrieval_corpus"]
        )
    assert cross.measurement.value > 0
    assert deleted.measurement.value > 0
    assert not all(item.passed for item in (*cross.outcomes, *deleted.outcomes))
