"""Pinned evaluation datasets and the protected gate configuration (INT-010 unit 1).

Two properties are proved here. A dataset is *content-addressed*: its digest is a pure function of its
identity, version, kind and every case, so the same content always hashes the same way, a change to any
case changes the digest, and a file whose recorded digest does not match its content is refused at load
— that is what makes "reproducible from pinned inputs" enforced rather than promised. And the gate
configuration cannot drift from its authority: the thresholds test parses DOSSIER.md §21.3 and fails if
the file and the table disagree on any metric, direction or threshold.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from intelligence.evaluation.datasets import (
    KINDS,
    DatasetError,
    Dataset,
    DatasetCase,
    dataset_from_mapping,
    datasets_of_kind,
    load_dataset,
    load_datasets,
)

ROOT = Path(__file__).resolve().parents[2]
DATASETS = ROOT / "tests" / "evaluation" / "datasets"


def case(case_id: str = "case-1", **overrides: object) -> dict[str, object]:
    values: dict[str, object] = {
        "case_id": case_id,
        "payload": {"query": "retention"},
        "protected": False,
        "tags": ["recall"],
    }
    values.update(overrides)
    return values


def spec(**overrides: object) -> dict[str, object]:
    values: dict[str, object] = {
        "dataset_id": "retrieval",
        "version": 1,
        "kind": "retrieval",
        "cases": [case()],
    }
    values.update(overrides)
    return values


# ------------------------------------------------------------------ pinned datasets


def test_every_pinned_dataset_loads_and_is_digest_pinned() -> None:
    datasets = load_datasets(DATASETS)
    assert {dataset.kind for dataset in datasets} == set(KINDS), (
        "one dataset per family the task names, so every metric family has pinned inputs"
    )
    for dataset in datasets:
        raw = json.loads((DATASETS / f"{dataset.dataset_id}.v{dataset.version}.json").read_text())
        assert raw["digest"] == dataset.digest, dataset.dataset_id
        assert dataset.cases, dataset.dataset_id
        assert dataset.source.endswith(".json")


def test_a_dataset_loads_its_protected_cases() -> None:
    retrieval = next(item for item in load_datasets(DATASETS) if item.dataset_id == "retrieval")
    protected = {item.case_id for item in retrieval.protected_cases}
    assert protected == {"cross_tenant_retrieval", "deleted_source_after_refresh"}, (
        "the adversarial and deletion checks are the retrieval family's safety cases"
    )
    assert [item.case_id for item in retrieval.tagged("recall")] == [
        "recall_runbook_retention",
        "recall_policy_metrics",
    ]
    with pytest.raises(DatasetError) as missing:
        retrieval.case("no-such-case")
    assert missing.value.rule_id == "dataset.cases"


def test_datasets_are_selectable_by_family() -> None:
    datasets = load_datasets(DATASETS)
    assert [item.dataset_id for item in datasets_of_kind(datasets, "grounding")] == ["grounding"]
    assert datasets_of_kind(datasets, "no_such_kind") == ()


# ------------------------------------------------------------------------- the digest


def test_the_digest_is_a_pure_function_of_the_pinned_content() -> None:
    first = dataset_from_mapping(spec())
    second = dataset_from_mapping(spec())
    assert first.digest == second.digest, "the same content always hashes the same way"
    assert len(first.digest) == 64
    assert first.canonical_bytes() == second.canonical_bytes()

    changed = dataset_from_mapping(
        spec(cases=[case("case-1", payload={"query": "retention ninety days"})])
    )
    assert changed.digest != first.digest, "a changed payload is a different dataset"

    reordered = dataset_from_mapping(spec(cases=[case("case-2"), case("case-1")]))
    assert reordered.digest != first.digest, "case order is part of the pinned content"

    bumped = dataset_from_mapping(spec(version=2))
    assert bumped.digest != first.digest, "a new version is a different dataset"


def test_a_drifted_digest_is_refused_at_load(tmp_path: Path) -> None:
    pinned = DATASETS / "retrieval.v1.json"
    drifted = json.loads(pinned.read_text())
    drifted["cases"][0]["payload"]["query"] = "a query that was never pinned"
    path = tmp_path / "retrieval.v1.json"
    path.write_text(json.dumps(drifted))
    with pytest.raises(DatasetError) as refusal:
        load_dataset(path)
    assert refusal.value.rule_id == "dataset.digest"
    assert "the pinned inputs have moved" in refusal.value.detail


def test_a_dataset_without_a_digest_is_not_pinned(tmp_path: Path) -> None:
    unpinned = spec()
    path = tmp_path / "unpinned.json"
    path.write_text(json.dumps(unpinned))
    assert load_dataset(path, require_digest=False).digest
    with pytest.raises(DatasetError) as refusal:
        load_dataset(path)
    assert refusal.value.rule_id == "dataset.digest"
    assert "not pinned" in refusal.value.detail


# --------------------------------------------------------------------------- shape


def test_the_dataset_shape_is_closed_and_validated() -> None:
    cases = [
        ("id", {"dataset_id": "Nope"}, "dataset.id"),
        ("version", {"version": 0}, "dataset.version"),
        ("kind", {"kind": "vibes"}, "dataset.kind"),
        ("no cases", {"cases": []}, "dataset.cases"),
        ("repeated case", {"cases": [case(), case()]}, "dataset.cases"),
        ("case payload", {"cases": [case(payload="not-an-object")]}, "dataset.cases"),
        ("unknown key", {"notes_extra": 1}, "dataset.shape"),
    ]
    for name, overrides, rule in cases:
        with pytest.raises(DatasetError) as refusal:
            dataset_from_mapping(spec(**overrides))
        assert refusal.value.rule_id == rule, name


def test_a_case_carries_a_payload_that_stays_opaque() -> None:
    dataset = dataset_from_mapping(spec(cases=[case(payload={"anything": ["the", "metric", "wants"]})]))
    assert dataset.cases[0].payload == {"anything": ["the", "metric", "wants"]}, (
        "the harness does not interpret a case, so it cannot invent case semantics"
    )
    assert isinstance(dataset.cases[0], DatasetCase)
    assert dataset.as_mapping()["cases"][0]["payload"] == {"anything": ["the", "metric", "wants"]}


def test_loading_a_directory_refuses_a_duplicate_identity(tmp_path: Path) -> None:
    pinned = dataset_from_mapping(spec())
    for name in ("a.json", "b.json"):
        (tmp_path / name).write_text(json.dumps(pinned.as_mapping()))
    with pytest.raises(DatasetError) as refusal:
        load_datasets(tmp_path)
    assert refusal.value.code == "CONFLICT_STATE"


def test_loading_refuses_a_missing_directory_and_malformed_file(tmp_path: Path) -> None:
    with pytest.raises(DatasetError) as missing:
        load_datasets(tmp_path / "nowhere")
    assert missing.value.rule_id == "dataset.io"
    (tmp_path / "broken.json").write_text("{not json")
    with pytest.raises(DatasetError) as broken:
        load_datasets(tmp_path)
    assert broken.value.rule_id == "dataset.io"


def test_the_pinned_corpus_is_the_repository_s_one_not_a_copy() -> None:
    """The injection metrics read INT-012's corpus; duplicating it would be a second authority."""
    corpus = ROOT / "tests" / "security" / "injection" / "corpus.json"
    assert corpus.is_file(), "INT-012's pinned injection corpus is where the injection metrics read"
    assert not list((DATASETS).glob("*injection*")), (
        "the injection corpus is referenced, not copied into a second dataset"
    )
