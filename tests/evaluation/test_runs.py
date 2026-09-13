"""The evaluation run record (INT-010 unit 2).

These prove why a run record exists: a measurement is only evidence if you can say what it measured and
what produced it. A run pins the datasets it used (identity, version, digest), the implementation
versions it ran under and its measurements; its digest is a pure function of exactly those, so two runs
of the same inputs are comparable and a run whose input moved is a different run. Comparability is a
check that names what differs, and cost/latency are recorded but deliberately excluded from it, because
they describe the environment rather than the measurement.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from intelligence.evaluation.datasets import load_datasets
from intelligence.evaluation.runs import (
    UNITS,
    CostLatency,
    DatasetPin,
    EvaluationRun,
    ImplementationVersions,
    Measurement,
    RunError,
    load_run,
    pins_for,
    run_for,
)

ROOT = Path(__file__).resolve().parents[2]
DATASETS = ROOT / "tests" / "evaluation" / "datasets"


@pytest.fixture(scope="module")
def dataset_set():
    return load_datasets(DATASETS)


def measurement(metric: str = "retrieval_recall_at_10", **overrides: object) -> Measurement:
    values: dict[str, object] = {"metric": metric, "value": 0.9, "unit": "ratio", "cases": 4}
    values.update(overrides)
    return Measurement(**values)  # type: ignore[arg-type]


def versions(**overrides: object) -> ImplementationVersions:
    values: dict[str, object] = {
        "model_route": "anthropic-sonnet",
        "provider": "anthropic",
        "embedding_route": "openai-text-embedding-3-small",
        "index_snapshot": "2026-09-13T00:00:00Z",
    }
    values.update(overrides)
    return ImplementationVersions(**values)  # type: ignore[arg-type]


def run(dataset_set, **overrides: object) -> EvaluationRun:
    values: dict[str, object] = {
        "measurements": [measurement()],
        "versions": versions(),
        "cost": CostLatency(wall_time_ms=1200, model_calls=3, input_tokens=900, output_tokens=120),
    }
    values.update(overrides)
    return run_for("run-1", dataset_set, **values)  # type: ignore[arg-type]


# --------------------------------------------------------------------------- pinning


def test_a_run_pins_the_datasets_it_measured(dataset_set) -> None:
    record = run(dataset_set)
    assert {pin.dataset_id for pin in record.pins} == {dataset.dataset_id for dataset in dataset_set}
    retrieval = record.pin("retrieval")
    assert retrieval.version == 1
    assert len(retrieval.digest) == 64
    assert retrieval.cases == 4
    assert record.pin("retrieval").digest == next(
        dataset.digest for dataset in dataset_set if dataset.dataset_id == "retrieval"
    ), "the pin is the digest the dataset was loaded at, not a restatement of it"

    with pytest.raises(RunError) as missing:
        record.pin("not_pinned")
    assert missing.value.rule_id == "run.pins"


def test_pins_are_taken_from_loaded_datasets(dataset_set) -> None:
    pins = pins_for(dataset_set)
    assert [pin.dataset_id for pin in pins] == [dataset.dataset_id for dataset in dataset_set]
    assert all(len(pin.digest) == 64 for pin in pins)


def test_a_run_must_pin_something() -> None:
    with pytest.raises(RunError) as refusal:
        EvaluationRun(
            run_id="run-1",
            pins=(),
            measurements=(measurement(),),
            versions=versions(),
            cost=CostLatency(wall_time_ms=1),
        )
    assert refusal.value.rule_id == "run.pins"
    assert "reproduced" in refusal.value.detail


def test_a_dataset_cannot_be_pinned_twice(dataset_set) -> None:
    with pytest.raises(RunError) as refusal:
        EvaluationRun(
            run_id="run-1",
            pins=(DatasetPin.of(dataset_set[0]), DatasetPin.of(dataset_set[0])),
            measurements=(measurement(),),
            versions=versions(),
            cost=CostLatency(wall_time_ms=1),
        )
    assert refusal.value.code == "CONFLICT_STATE"


# ------------------------------------------------------------------------ measurement


def test_a_measurement_is_validated_in_the_unit_the_gate_uses() -> None:
    assert UNITS == ("ratio", "count")
    assert measurement().unit == "ratio"
    assert measurement(metric="cross_tenant_retrieval", value=0, unit="count").value == 0
    cases = [
        ("empty metric", {"metric": " "}, "run.measurement"),
        ("non-finite", {"value": float("nan")}, "run.measurement"),
        ("unknown unit", {"unit": "furlongs"}, "run.measurement"),
        ("ratio out of range", {"value": 1.5}, "run.measurement"),
        ("negative count", {"metric": "x", "value": -1, "unit": "count"}, "run.measurement"),
        ("negative cases", {"cases": -1}, "run.measurement"),
    ]
    for name, overrides, rule in cases:
        with pytest.raises(RunError) as refusal:
            measurement(**overrides)
        assert refusal.value.rule_id == rule, name


def test_the_same_metric_cannot_be_measured_twice(dataset_set) -> None:
    with pytest.raises(RunError) as refusal:
        run(dataset_set, measurements=[measurement(), measurement()])
    assert refusal.value.code == "CONFLICT_STATE"


def test_a_missing_measurement_is_reported_rather_than_defaulted(dataset_set) -> None:
    record = run(dataset_set, measurements=[measurement("retrieval_recall_at_10")])
    assert record.measurement("retrieval_recall_at_10") is not None
    assert record.measurement("route_determinism") is None, (
        "an unmeasured metric must be absent, so the gate can fail closed on it"
    )


# --------------------------------------------------------------------------- cost


def test_cost_and_latency_are_recorded_and_validated() -> None:
    cost = CostLatency(
        wall_time_ms=2500, model_calls=4, input_tokens=1000, output_tokens=200, cost_minor_units=7
    )
    assert cost.as_mapping()["wall_time_ms"] == 2500
    with pytest.raises(RunError) as refusal:
        CostLatency(wall_time_ms=-1)
    assert refusal.value.rule_id == "run.cost"


# ------------------------------------------------------------------- reproducibility


def test_the_run_digest_is_a_pure_function_of_what_was_measured(dataset_set) -> None:
    first = run(dataset_set)
    second = run(dataset_set)
    assert first.digest == second.digest, "the same measurement under the same versions is the same run"

    assert run(dataset_set, measurements=[measurement(value=0.91)]).digest != first.digest
    assert run(dataset_set, versions=versions(model_route="openai-gpt")).digest != first.digest
    assert run(
        dataset_set, cost=CostLatency(wall_time_ms=99_999)
    ).digest == first.digest, "cost is an observation about the environment, not the measurement"


def test_comparability_names_what_differs(dataset_set) -> None:
    baseline = run(dataset_set)
    candidate = run(dataset_set, measurements=[measurement(value=0.95)])
    assert baseline.comparable_with(candidate) == (True, ""), (
        "a quality change under the same inputs and versions is a comparable run"
    )

    moved_version = run(dataset_set, versions=versions(embedding_route="other-route"))
    comparable, reason = baseline.comparable_with(moved_version)
    assert not comparable and "embedding_route" in reason

    moved_inputs = EvaluationRun(
        run_id="run-2",
        pins=(DatasetPin(dataset_id="retrieval", version=2, digest="a" * 64, cases=5),),
        measurements=(measurement(),),
        versions=versions(),
        cost=CostLatency(wall_time_ms=1),
    )
    comparable, reason = baseline.comparable_with(moved_inputs)
    assert not comparable
    assert "pinned only by" in reason, "a run that measured a different set of datasets is not comparable"


def test_skill_versions_are_compared_only_when_asked(dataset_set) -> None:
    first = run(dataset_set, versions=versions(skill_versions=(("sk-1", "1.0.0"),)))
    second = run(dataset_set, versions=versions(skill_versions=(("sk-1", "1.1.0"),)))
    assert first.comparable_with(second) == (True, ""), (
        "a routing run does not depend on skill versions, so they do not make it incomparable"
    )
    comparable, reason = first.comparable_with(second, skills=True)
    assert not comparable and "skill_versions" in reason


# ---------------------------------------------------------------------------- record


def test_a_run_round_trips_through_its_record(tmp_path: Path, dataset_set) -> None:
    record = run(dataset_set, started_at="2026-09-13T00:00:00Z", finished_at="2026-09-13T00:01:00Z")
    path = tmp_path / "run.json"
    path.write_text(json.dumps(record.as_mapping(), indent=2))
    loaded = load_run(path)
    assert loaded.digest == record.digest
    assert loaded.measurements == record.measurements
    assert loaded.versions == record.versions
    assert loaded.cost == record.cost
    assert loaded.pins == record.pins


def test_a_drifted_run_record_is_refused(tmp_path: Path, dataset_set) -> None:
    record = run(dataset_set)
    document = record.as_mapping()
    document["measurements"][0]["value"] = 0.5  # type: ignore[index]
    path = tmp_path / "run.json"
    path.write_text(json.dumps(document))
    with pytest.raises(RunError) as refusal:
        load_run(path)
    assert refusal.value.rule_id == "run.shape"
    assert "hashes to" in refusal.value.detail


def test_a_malformed_run_record_fails_closed(tmp_path: Path, dataset_set) -> None:
    path = tmp_path / "run.json"
    path.write_text(json.dumps({"run_id": "run-1", "pins": "not-a-list"}))
    with pytest.raises(RunError) as refusal:
        load_run(path)
    assert refusal.value.rule_id == "run.shape"
    path.write_text("{not json")
    with pytest.raises(RunError) as broken:
        load_run(path)
    assert broken.value.rule_id == "run.shape"
    with pytest.raises(RunError) as missing:
        load_run(tmp_path / "absent.json")
    assert missing.value.code == "NOT_FOUND"


def test_a_run_shape_is_validated() -> None:
    with pytest.raises(RunError) as nameless:
        EvaluationRun(
            run_id="  ",
            pins=(DatasetPin(dataset_id="retrieval", version=1, digest="b" * 64),),
            measurements=(measurement(),),
            versions=versions(),
            cost=CostLatency(wall_time_ms=1),
        )
    assert nameless.value.rule_id == "run.id"

    with pytest.raises(RunError) as backwards:
        EvaluationRun(
            run_id="run-1",
            pins=(DatasetPin(dataset_id="retrieval", version=1, digest="b" * 64),),
            measurements=(measurement(),),
            versions=versions(),
            cost=CostLatency(wall_time_ms=1),
            started_at="2026-09-13T00:02:00Z",
            finished_at="2026-09-13T00:01:00Z",
        )
    assert backwards.value.rule_id == "run.shape"


def test_repeated_skill_versions_are_refused() -> None:
    with pytest.raises(RunError) as refusal:
        versions(skill_versions=(("sk-1", "1.0.0"), ("sk-1", "1.1.0")))
    assert refusal.value.code == "CONFLICT_STATE"
    with pytest.raises(RunError) as blank:
        versions(skill_versions=(("sk-1", "  "),))
    assert blank.value.rule_id == "run.versions"
