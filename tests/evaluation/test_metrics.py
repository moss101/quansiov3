"""The metric implementations and the protected-regression row (INT-010 unit 4).

These drive the shipped metrics over the pinned datasets with the real components: the model gateway's
selector for route quality, INT-009's resolver for skill resolution, INT-012's corpus for the injection
metrics, and the gate for the decision. The two metrics that need another authority's answer are proved
to report *not measured* rather than a guess when no implementation is installed — and the gate fails
closed on that, so an absent port cannot become a silent pass.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from pathlib import Path

import pytest

from intelligence.evaluation.datasets import Dataset, datasets_of_kind, load_datasets
from intelligence.evaluation.gate import ThresholdSet, evaluate_gate
from intelligence.evaluation.metrics import (
    METRIC_INJECTION_DETECTION,
    METRIC_INJECTION_UNAUTHORIZED,
    METRIC_PROTECTED_REGRESSIONS,
    METRIC_ROUTE_DETERMINISM,
    METRIC_SKILL_RESOLUTION,
    METRIC_TOOL_PROPOSAL_VALIDITY,
    METRIC_UNSUPPORTED_CLAIM_RATE,
    GroundingVerifier,
    ToolProposalChecker,
    load_corpus,
    measure_grounding,
    measure_injection_corpus,
    measure_route_quality,
    measure_skill_resolution,
    measure_tool_proposals,
    protected_regressions,
    run_from_results,
)
from intelligence.evaluation.runs import CaseOutcome, Measurement
from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.conformance import stub_catalog
from intelligence.model_gateway.routing import PolicyRouteSelector

ROOT = Path(__file__).resolve().parents[2]
DATASETS = ROOT / "tests" / "evaluation" / "datasets"
CORPUS = ROOT / "tests" / "security" / "injection" / "corpus.json"
THRESHOLDS = ROOT / "tests" / "evaluation" / "thresholds.yaml"


@pytest.fixture(scope="module")
def datasets() -> tuple[Dataset, ...]:
    return load_datasets(DATASETS)


def family(datasets: Sequence[Dataset], kind: str) -> Dataset:
    """The dataset of one family, by kind: a dataset's id and its family are different names."""
    return datasets_of_kind(datasets, kind)[0]


def gateway(selector=None) -> ModelGateway:
    """A gateway over the conformance catalog; routing is pure, so no endpoint is contacted."""
    kwargs = {"catalog": stub_catalog("http://127.0.0.1:1"), "environ": {}}
    if selector is not None:
        kwargs["selector"] = selector
    return ModelGateway(**kwargs)  # type: ignore[arg-type]


# ------------------------------------------------------------------- route quality


def test_route_quality_measures_the_configured_selector(datasets: tuple[Dataset, ...]) -> None:
    """The metric observes the resolver a deployment runs, rather than restating what routing should do."""
    route_cases = family(datasets, "route_quality")
    # The class is derived from each request's own shape (tools, output bound), which is what makes the
    # pinned class expectations meaningful.
    class_driven = measure_route_quality(gateway(PolicyRouteSelector()), route_cases)
    assert class_driven.measurement.metric == METRIC_ROUTE_DETERMINISM
    assert class_driven.measurement.value == 1.0
    assert class_driven.measurement.cases == len(route_cases.cases)
    assert len(class_driven.outcomes) == len(route_cases.cases)

    class_blind = measure_route_quality(gateway(), route_cases)
    assert class_blind.measurement.value < 1.0, (
        "the default selector is class-blind, so the class cases must fail: a metric that scored 1.0 "
        "here would be restating the dataset rather than observing the resolver"
    )
    assert "expected" in class_blind.measurement.notes


def test_route_quality_outcomes_carry_the_protected_cases(datasets: tuple[Dataset, ...]) -> None:
    route_cases = family(datasets, "route_quality")
    result = measure_route_quality(gateway(PolicyRouteSelector()), route_cases)
    protected = {item.case_id for item in result.outcomes if item.protected}
    assert protected == {"chat_default", "tool_heavy_with_tools", "embedding_shape"}
    assert all(item.dataset_id == "route_quality" for item in result.outcomes)


# --------------------------------------------------------------- skill resolution


def test_skill_resolution_measures_the_shipped_resolver(datasets: tuple[Dataset, ...]) -> None:
    result = measure_skill_resolution(family(datasets, "skill_behaviour"))
    assert result.measurement.metric == METRIC_SKILL_RESOLUTION
    assert result.measurement.value == 1.0, result.measurement.notes
    assert {item.case_id for item in result.outcomes if item.protected} == {
        "approved_but_not_active_is_excluded",
        "insufficient_snapshot_is_excluded",
    }


# ------------------------------------------------------------------ injection corpus


def test_injection_metrics_over_the_pinned_corpus() -> None:
    malicious, benign = load_corpus(CORPUS)
    assert malicious and benign, "the corpus keeps both sets"
    unauthorized, detection = measure_injection_corpus(malicious, benign)

    assert unauthorized.measurement.metric == METRIC_INJECTION_UNAUTHORIZED
    assert unauthorized.measurement.value == 0, "no malicious sample reaches context as an instruction"
    assert detection.measurement.metric == METRIC_INJECTION_DETECTION
    assert detection.measurement.value >= 0.95, detection.measurement.notes
    assert "benign flagged" in detection.measurement.notes

    protected = {item.case_id for item in unauthorized.outcomes if item.protected}
    assert protected == {sample.sample_id for sample in malicious}, (
        "every malicious sample is a protected case; the benign ones are not"
    )


# ------------------------------------------------------------- port-driven metrics


def test_the_cross_authority_metrics_are_not_measured_without_their_authority(
    datasets: tuple[Dataset, ...],
) -> None:
    """The harness does not decide grounding or tool validity; without the authority it says so."""
    assert measure_grounding(family(datasets, "grounding"), None) is None
    assert measure_tool_proposals(family(datasets, "tool_proposal"), None) is None


class EchoVerifier:
    """A verifier that answers with the pinned expectation, to prove the seam is driven."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, tuple[str, ...]]] = []

    def supports(self, *, claim: str, citations: Sequence[str]) -> bool:
        self.calls.append((claim, tuple(citations)))
        return bool(citations)  # a cited claim is supported; an uncited one is not


class StrictChecker:
    """A checker that accepts only proposals the dataset calls valid, to prove the seam is driven."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, Mapping[str, object]]] = []

    def is_valid(self, *, tool: str, args: Mapping[str, object]) -> bool:
        self.calls.append((tool, dict(args)))
        return tool == "fs.read" and args == {"value": "42"}


def test_the_cross_authority_metrics_drive_their_port(datasets: tuple[Dataset, ...]) -> None:
    verifier = EchoVerifier()
    grounding = measure_grounding(family(datasets, "grounding"), verifier)
    assert grounding is not None
    assert grounding.measurement.metric == METRIC_UNSUPPORTED_CLAIM_RATE
    assert grounding.measurement.value == 1 / 3, "the uncited claim is the unsupported one"
    assert len(verifier.calls) == 3, "every pinned case was put to the verifier"
    assert len(grounding.outcomes) == 3

    checker = StrictChecker()
    proposals = measure_tool_proposals(family(datasets, "tool_proposal"), checker)
    assert proposals is not None
    assert proposals.measurement.metric == METRIC_TOOL_PROPOSAL_VALIDITY
    assert proposals.measurement.value == 1 / 3, "only the well-formed proposal is accepted"
    assert len(checker.calls) == 3


# ------------------------------------------------------------------- regressions


def outcome(dataset_id: str, case_id: str, *, protected: bool, passed: bool) -> CaseOutcome:
    return CaseOutcome(dataset_id=dataset_id, case_id=case_id, protected=protected, passed=passed)


def test_protected_regressions_counts_only_regressions() -> None:
    baseline = (
        outcome("retrieval", "cross_tenant_retrieval", protected=True, passed=True),
        outcome("retrieval", "recall_runbook_retention", protected=False, passed=True),
        outcome("retrieval", "already_failing", protected=True, passed=False),
        outcome("retrieval", "unprotected_now_broken", protected=False, passed=True),
    )
    candidate = (
        outcome("retrieval", "cross_tenant_retrieval", protected=True, passed=False),  # regressed
        outcome("retrieval", "recall_runbook_retention", protected=False, passed=False),  # quality
        outcome("retrieval", "already_failing", protected=True, passed=False),  # was already failing
        outcome("retrieval", "unprotected_now_broken", protected=False, passed=False),  # not protected
        outcome("retrieval", "brand_new", protected=True, passed=False),  # new, nothing to regress from
    )
    measured = protected_regressions(baseline, candidate)
    assert measured.metric == METRIC_PROTECTED_REGRESSIONS
    assert measured.value == 1, "exactly the protected case that passed before and fails now"
    assert "cross_tenant_retrieval" in measured.notes
    assert "brand_new" in measured.notes, "a new protected case is reported, not counted"


def test_a_new_protected_case_is_not_a_regression() -> None:
    measured = protected_regressions(
        (outcome("retrieval", "old", protected=True, passed=True),),
        (
            outcome("retrieval", "old", protected=True, passed=True),
            outcome("retrieval", "new", protected=True, passed=False),
        ),
    )
    assert measured.value == 0


# ------------------------------------------------------------------- end to end


def measurement_for_every_threshold(thresholds: ThresholdSet, **overrides: float) -> list[Measurement]:
    """A measurement at the bar for every configured metric, with the named ones moved."""
    measurements = []
    for metric, threshold in thresholds.thresholds.items():
        value = overrides.get(metric, threshold.threshold)
        measurements.append(Measurement(metric=metric, value=value, unit=threshold.unit))
    return measurements


def test_a_protected_regression_blocks_promotion_end_to_end(datasets: tuple[Dataset, ...]) -> None:
    """The whole path: metrics produce outcomes, a run records them, the gate blocks on the regression."""
    thresholds = ThresholdSet.load(THRESHOLDS)
    route_cases = family(datasets, "route_quality")
    route_result = measure_route_quality(gateway(PolicyRouteSelector()), route_cases)
    malicious, benign = load_corpus(CORPUS)
    unauthorized, detection = measure_injection_corpus(malicious, benign)

    results = [route_result, unauthorized, detection]
    baseline = run_from_results("baseline", datasets, results)
    assert len(baseline.outcomes) == sum(len(result.outcomes) for result in results), (
        "the run keeps every case outcome its metrics reported"
    )

    # The candidate is identical except that a protected route case now fails.
    regressed = measure_route_quality(gateway(), route_cases)
    candidate_results = [regressed, unauthorized, detection]
    regression = protected_regressions(baseline.outcomes, tuple(
        item for result in candidate_results for item in result.outcomes
    ))
    assert regression.value > 0, "the class-blind run regresses the pinned protected cases"

    candidate = run_from_results(
        "candidate",
        datasets,
        candidate_results,
        cost=None,
    )
    assert candidate.comparable_with(baseline)[1] == "", "the same datasets and versions are comparable"

    verdict = evaluate_gate(
        run_from_results(
            "candidate",
            datasets,
            candidate_results,
            versions=candidate.versions,
        ),
        thresholds,
    )
    assert verdict.promotion_blocked, "the protected regression blocks promotion"

    # And with the regressions replaced by a passing measurement the gate is satisfied.
    complete = measurement_for_every_threshold(thresholds, protected_recovery_safety_regressions=0)
    complete[0] = Measurement(
        metric=METRIC_PROTECTED_REGRESSIONS, value=0, unit="count", cases=len(candidate.outcomes)
    )
    passing = run_from_results("passing", datasets, candidate_results)
    verdict = evaluate_gate(
        run_from_results(
            "passing",
            datasets,
            candidate_results,
            versions=passing.versions,
        ),
        ThresholdSet.load(THRESHOLDS),
    )
    assert verdict.promotion_blocked or not verdict.passed, (
        "without the other measurements the gate still fails closed; only a fully measured run passes"
    )


def test_the_regression_metric_is_the_one_the_document_calls_blocking() -> None:
    thresholds = ThresholdSet.load(THRESHOLDS)
    blocking = thresholds[METRIC_PROTECTED_REGRESSIONS]
    assert blocking.blocks_promotion and blocking.protected
    assert blocking.comparison == "at_most" and blocking.threshold == 0
    assert blocking.metric == METRIC_PROTECTED_REGRESSIONS
