"""The decision gate (INT-010 unit 3, DOSSIER §21.3).

This is where the task's second acceptance statement becomes executable: a protected safety or recovery
regression blocks promotion **regardless of aggregate quality gain**, so the gate is not a weighted
score. These tests prove that, the fail-closed behaviour on missing and mis-unit measurements, and that
a verdict records the configuration that decided it.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from intelligence.evaluation.datasets import load_datasets
from intelligence.evaluation.gate import (
    COMPARISONS,
    GateThreshold,
    MetricVerdict,
    ThresholdError,
    ThresholdSet,
    evaluate_gate,
)
from intelligence.evaluation.runs import (
    CostLatency,
    EvaluationRun,
    ImplementationVersions,
    Measurement,
    run_for,
)

ROOT = Path(__file__).resolve().parents[2]
THRESHOLDS = ROOT / "tests" / "evaluation" / "thresholds.yaml"
DATASETS = ROOT / "tests" / "evaluation" / "datasets"


@pytest.fixture(scope="module")
def thresholds() -> ThresholdSet:
    return ThresholdSet.load(THRESHOLDS)


def make_thresholds(**metrics: tuple[str, float] | tuple[str, float, bool]) -> ThresholdSet:
    """A small configuration: `metric=(comparison, threshold[, protected])`."""
    configured: dict[str, GateThreshold] = {}
    for metric, spec in metrics.items():
        comparison, value = spec[0], spec[1]
        protected = bool(spec[2]) if len(spec) > 2 else False
        configured[metric] = GateThreshold(
            metric=metric, comparison=comparison, threshold=value, unit="ratio", protected=protected
        )
    return ThresholdSet(version=1, ratified=False, source="test configuration", thresholds=configured)


def make_run(*measurements: Measurement) -> EvaluationRun:
    datasets = load_datasets(DATASETS)
    return run_for(
        "run-1",
        datasets,
        measurements=measurements,
        versions=ImplementationVersions(model_route="anthropic-sonnet"),
        cost=CostLatency(wall_time_ms=100),
    )


def ratio(metric: str, value: float) -> Measurement:
    return Measurement(metric=metric, value=value, unit="ratio")


def count(metric: str, value: int) -> Measurement:
    return Measurement(metric=metric, value=value, unit="count")


def gated_run(thresholds: ThresholdSet, **overrides: float) -> EvaluationRun:
    """A run measuring every configured metric at the bar, with the named ones moved.

    Measuring all of them is deliberate: a metric the run does not measure fails closed, so a suite
    that left them out would be testing the fail-closed path rather than the metric under test.
    """
    measurements: dict[str, Measurement] = {}
    for metric, threshold in thresholds.thresholds.items():
        value = overrides.get(metric, threshold.threshold)
        measurements[metric] = Measurement(metric=metric, value=value, unit=threshold.unit)
    unknown = set(overrides) - set(thresholds.thresholds)
    assert not unknown, f"the configuration does not gate {sorted(unknown)}"
    return make_run(*measurements.values())


# ---------------------------------------------------------------------- the authority


def test_the_shipped_configuration_loads_and_gates_a_run(thresholds: ThresholdSet) -> None:
    assert thresholds.version >= 1
    assert thresholds.ratified is False, "§21.3 asks for owner ratification before this is a release bar"
    assert "21.3" in thresholds.source
    verdict = evaluate_gate(gated_run(thresholds), thresholds)
    assert verdict.passed and not verdict.promotion_blocked
    assert len(verdict.verdicts) == 9, "every configured metric is decided"
    assert verdict.verdicts[0].protected, "protected metrics are decided first"
    assert verdict.thresholds_version == thresholds.version
    assert verdict.ratified is False
    assert verdict.source == thresholds.source


def test_a_protected_regression_blocks_promotion_whatever_the_quality_says(
    thresholds: ThresholdSet,
) -> None:
    """The acceptance statement, stated directly: quality cannot buy off safety."""
    verdict = evaluate_gate(
        gated_run(
            thresholds,
            # every quality metric at its best, and one tenant's row leaked anyway
            tool_proposal_schema_validity=1.0,
            unsupported_claim_rate=0.0,
            retrieval_recall_at_10=1.0,
            injection_escalation_detection_rate=1.0,
            cross_tenant_retrieval=1,
        ),
        thresholds,
    )
    assert not verdict.passed
    assert verdict.promotion_blocked
    assert [item.metric for item in verdict.blockers] == ["cross_tenant_retrieval"]
    assert verdict.failures == (), "the quality metrics all held; only the protected one did not"
    assert "cannot trade" in verdict.reason
    assert all(item.passed for item in verdict.verdicts if not item.protected), (
        "every quality metric passed and promotion is still blocked"
    )


def test_a_quality_regression_fails_the_gate_without_blocking_promotion(
    thresholds: ThresholdSet,
) -> None:
    verdict = evaluate_gate(gated_run(thresholds, retrieval_recall_at_10=0.5), thresholds)
    assert not verdict.passed, "a stated bar that was missed has not been met"
    assert not verdict.promotion_blocked, "a quality miss is a failure, not a safety block"
    assert [item.metric for item in verdict.failures] == ["retrieval_recall_at_10"]
    assert "quality metric" in verdict.reason


def test_the_quality_failure_does_not_hide_the_protected_one(thresholds: ThresholdSet) -> None:
    """Both are reported, and the protected one is what blocks."""
    verdict = evaluate_gate(
        gated_run(thresholds, retrieval_recall_at_10=0.5, deleted_memory_retrieval_after_refresh=2),
        thresholds,
    )
    assert [item.metric for item in verdict.blockers] == ["deleted_memory_retrieval_after_refresh"]
    assert [item.metric for item in verdict.failures] == ["retrieval_recall_at_10"]
    assert verdict.promotion_blocked


# ------------------------------------------------------------------------ fail closed


def test_an_unmeasured_metric_fails_closed(thresholds: ThresholdSet) -> None:
    verdict = evaluate_gate(make_run(ratio("route_determinism", 1.0)), thresholds)
    assert not verdict.passed
    assert verdict.promotion_blocked, "an unmeasured protected metric is not a pass"
    missing = [item for item in verdict.verdicts if not item.measured]
    assert missing, "the unmeasured metrics are reported"
    assert all(not item.passed for item in missing)
    assert "not measured" in missing[0].detail


def test_a_mis_unit_measurement_fails_closed() -> None:
    """A count compared against a ratio threshold would be meaningless, so it fails."""
    thresholds = make_thresholds(ratio_metric=("at_least", 0.9))
    verdict = evaluate_gate(
        make_run(Measurement(metric="ratio_metric", value=1, unit="count")),
        thresholds,
    )
    assert not verdict.passed
    assert "measured as count" in verdict.verdicts[0].detail


def test_an_unknown_metric_is_reported_as_ungated(thresholds: ThresholdSet) -> None:
    verdict = evaluate_gate(
        make_run(ratio("route_determinism", 1.0), ratio("something_new", 0.5)),
        thresholds,
    )
    assert verdict.ungated == ("something_new",), (
        "a measurement the configuration does not gate is reported, not silently ignored"
    )


# --------------------------------------------------------------------------- authority


def test_the_configuration_shape_is_closed() -> None:
    with pytest.raises(ThresholdError) as no_metrics:
        ThresholdSet.from_mapping({"version": 1, "source": "x", "metrics": {}})
    assert no_metrics.value.rule_id == "thresholds.shape"
    with pytest.raises(ThresholdError) as bad_version:
        ThresholdSet.from_mapping(
            {"version": 0, "source": "x", "metrics": {"m": {"comparison": "at_least", "threshold": 1, "unit": "ratio"}}}
        )
    assert bad_version.value.rule_id == "thresholds.shape"
    with pytest.raises(ThresholdError) as unknown_comparison:
        ThresholdSet.from_mapping(
            {"version": 1, "source": "x", "metrics": {"m": {"comparison": "roughly", "threshold": 1, "unit": "ratio"}}}
        )
    assert unknown_comparison.value.rule_id == "thresholds.comparison"
    with pytest.raises(ThresholdError) as no_threshold:
        ThresholdSet.from_mapping(
            {"version": 1, "source": "x", "metrics": {"m": {"comparison": "at_least", "unit": "ratio"}}}
        )
    assert no_threshold.value.rule_id == "thresholds.metric"
    with pytest.raises(ThresholdError) as no_source:
        ThresholdSet(version=1, ratified=False, source="  ", thresholds={"m": GateThreshold("m", "at_least", 1, "ratio")})
    assert no_source.value.rule_id == "thresholds.shape"
    assert COMPARISONS == ("at_least", "at_most")


def test_only_a_protected_metric_may_block_promotion() -> None:
    with pytest.raises(ThresholdError) as refusal:
        GateThreshold(
            metric="m", comparison="at_least", threshold=1, unit="ratio", protected=False, blocks_promotion=True
        )
    assert refusal.value.rule_id == "thresholds.shape"
    assert "only a protected metric" in refusal.value.detail


def test_the_blocking_row_blocks_even_when_every_other_metric_holds(thresholds: ThresholdSet) -> None:
    verdict = evaluate_gate(gated_run(thresholds, protected_recovery_safety_regressions=1), thresholds)
    assert verdict.promotion_blocked
    assert [item.metric for item in verdict.blockers] == ["protected_recovery_safety_regressions"]


def test_a_verdict_records_the_configuration_that_decided_it(thresholds: ThresholdSet) -> None:
    verdict = evaluate_gate(make_run(ratio("route_determinism", 1.0)), thresholds)
    document = verdict.as_mapping()
    assert document["thresholds"] == {
        "version": thresholds.version,
        "ratified": thresholds.ratified,
        "source": thresholds.source,
    }
    assert document["passed"] is False and document["promotion_blocked"] is True
    assert all(isinstance(item, MetricVerdict) for item in verdict.verdicts)
    assert set(document["blockers"][0]) == {
        "metric",
        "protected",
        "measured",
        "value",
        "threshold",
        "comparison",
        "passed",
        "detail",
    }


def test_loading_a_missing_or_malformed_configuration_fails_closed(tmp_path: Path) -> None:
    with pytest.raises(ThresholdError) as missing:
        ThresholdSet.load(tmp_path / "absent.yaml")
    assert missing.value.code == "NOT_FOUND"
    (tmp_path / "broken.yaml").write_text("metrics: [not, a, mapping]\n")
    with pytest.raises(ThresholdError) as broken:
        ThresholdSet.load(tmp_path / "broken.yaml")
    assert broken.value.rule_id == "thresholds.shape"
