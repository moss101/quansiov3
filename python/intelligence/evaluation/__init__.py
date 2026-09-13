"""Intelligence evaluation harness: make model/context/skill changes measurable before promotion
(INT-010, DOSSIER §21).

The harness answers five questions before a change is promoted — route quality, retrieval grounding,
answer grounding, tool-proposal validity and skill behaviour — and it *measures and gates*: it never
promotes. Promotion is the operator's and REL-001's, so a gate's output is a decision record, never a
mutation of any registry or product state.

Layout:

* `datasets` — versioned, content-addressed datasets: the pinned inputs a run is reproducible from, and
  a load that refuses drift from the recorded digest;
* `semantic_verifier` — RUN-008's independent completion verifier, which the runtime drives (not a
  metric of this harness).

The protected gate configuration lives in `tests/evaluation/thresholds.yaml`, which encodes
DOSSIER §21.3 verbatim and is checked against the document by `tests/evaluation/test_thresholds.py`.
The run record and the decision gate are the remaining units of this task.
"""

from __future__ import annotations

from intelligence.evaluation.datasets import (
    KINDS,
    Dataset,
    DatasetCase,
    DatasetError,
    dataset_from_mapping,
    datasets_of_kind,
    load_dataset,
    load_datasets,
)

__all__ = [
    "KINDS",
    "Dataset",
    "DatasetCase",
    "DatasetError",
    "dataset_from_mapping",
    "datasets_of_kind",
    "load_dataset",
    "load_datasets",
]
