"""Intelligence evaluation harness: make model/context/skill changes measurable before promotion
(INT-010, DOSSIER §21).

The harness answers five questions before a change is promoted — route quality, retrieval grounding,
answer grounding, tool-proposal validity and skill behaviour — and it *measures and gates*: it never
promotes. Promotion is the operator's and REL-001's, so a gate's output is a decision record, never a
mutation of any registry or product state.

Layout:

* `datasets` — versioned, content-addressed datasets: the pinned inputs a run is reproducible from, and
  a load that refuses drift from the recorded digest;
* `runs` — the record of one run: its dataset pins, the versions it ran under, its measurements and its
  cost/latency, content-addressed so two runs of the same inputs are comparable;
* `gate` — the decision: protected metrics first and blocking, quality metrics reported, and a verdict
  that records the configuration it applied;
* `metrics` — the measurements: the five dataset families turned into measurements and per-case
  outcomes, with the two cross-authority metrics port-driven and reported as not measured when the
  authority is absent;
* `semantic_verifier` — RUN-008's independent completion verifier, which the runtime drives (not a
  metric of this harness).

The protected gate configuration lives in `tests/evaluation/thresholds.yaml`, which encodes
DOSSIER §21.3 verbatim and is checked against the document by `tests/evaluation/test_thresholds.py`.
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
from intelligence.evaluation.gate import (
    COMPARISONS,
    GateThreshold,
    GateVerdict,
    MetricVerdict,
    ThresholdError,
    ThresholdSet,
    evaluate_gate,
)
from intelligence.evaluation.metrics import (
    METRIC_CROSS_TENANT,
    METRIC_DELETED_AFTER_REFRESH,
    METRIC_INJECTION_DETECTION,
    METRIC_INJECTION_UNAUTHORIZED,
    METRIC_PROTECTED_REGRESSIONS,
    METRIC_RETRIEVAL_RECALL,
    METRIC_ROUTE_DETERMINISM,
    METRIC_SKILL_RESOLUTION,
    METRIC_TOOL_PROPOSAL_VALIDITY,
    METRIC_UNSUPPORTED_CLAIM_RATE,
    GroundingVerifier,
    MetricResult,
    ToolProposalChecker,
    load_corpus,
    measure_grounding,
    measure_injection_corpus,
    measure_retrieval,
    measure_route_quality,
    measure_skill_resolution,
    measure_tool_proposals,
    protected_regressions,
    run_from_results,
)
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

__all__ = [
    "COMPARISONS",
    "KINDS",
    "METRIC_CROSS_TENANT",
    "METRIC_DELETED_AFTER_REFRESH",
    "METRIC_INJECTION_DETECTION",
    "METRIC_INJECTION_UNAUTHORIZED",
    "METRIC_PROTECTED_REGRESSIONS",
    "METRIC_RETRIEVAL_RECALL",
    "METRIC_ROUTE_DETERMINISM",
    "METRIC_SKILL_RESOLUTION",
    "METRIC_TOOL_PROPOSAL_VALIDITY",
    "METRIC_UNSUPPORTED_CLAIM_RATE",
    "UNITS",
    "CostLatency",
    "Dataset",
    "DatasetCase",
    "DatasetError",
    "DatasetPin",
    "EvaluationRun",
    "GateThreshold",
    "GateVerdict",
    "GroundingVerifier",
    "ImplementationVersions",
    "Measurement",
    "MetricResult",
    "MetricVerdict",
    "RunError",
    "ThresholdError",
    "ThresholdSet",
    "ToolProposalChecker",
    "dataset_from_mapping",
    "datasets_of_kind",
    "evaluate_gate",
    "load_corpus",
    "load_dataset",
    "load_datasets",
    "load_run",
    "measure_grounding",
    "measure_injection_corpus",
    "measure_retrieval",
    "measure_route_quality",
    "measure_skill_resolution",
    "measure_tool_proposals",
    "pins_for",
    "protected_regressions",
    "run_for",
    "run_from_results",
]
