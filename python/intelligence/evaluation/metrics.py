"""The metrics a run reports (INT-010 unit 4, DOSSIER §21.3).

Each function here turns a pinned dataset into a `MetricResult`: the aggregate `Measurement` the gate
thresholds, plus the per-case outcome of every case. The per-case outcomes are what make DOSSIER §21.3's
last row measurable — *protected recovery/safety regressions: 0* is a statement about individual
protected cases across runs, not about an average, so a run has to remember which cases passed.

Every metric measures something this plane can actually observe:

* `measure_route_quality` resolves each pinned request through the model gateway's selector — the one the
  deployment runs — and reports the share that resolved deterministically *and* to the class the dataset
  pins. It observes the configured resolver rather than restating what routing should do, so a deployment
  running a class-blind selector scores below 1.0 and the metric says why.
* `measure_skill_resolution` drives INT-009's resolver over the pinned cases: only `ACTIVE` versions may
  resolve, and a version needing authority the caller does not hold is excluded, never granted.
* `measure_injection_corpus` reads INT-012's corpus through the shipped heuristics: the escalation rate,
  and the count of malicious samples that would reach context *as an instruction* rather than as data.
  This plane executes no effects, so the effect-level check belongs to the runtime's escalation path
  (INT-012/RUN-006); what is measured here is the precondition an unauthorized effect would need.
* `measure_retrieval` drives a real derived index: recall@10 over a pinned corpus, and the two counts
  DOSSIER §21.3 requires to be zero — another tenant's rows, and a deleted source still retrievable after
  the index refreshed.

Two metrics need another authority's answer and are therefore **ports, not inventions**: answer grounding
needs a verifier (RUN-008's seam), and tool-proposal validity needs the tool registry's own schema check
(RUN-011, Rust). With no implementation installed the metric is reported as **not measured** rather than
guessed, and the gate fails closed on an unmeasured thresholded metric — so an absent port cannot become
a silent pass.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from quansio.v1.intelligence import intelligence_pb2

from intelligence.embeddings.index import EmbeddingIndex, SourceDocument
from intelligence.embeddings.sources import SOURCE_KIND_ARTIFACT
from intelligence.evaluation.datasets import Dataset, DatasetCase
from intelligence.evaluation.runs import (
    CaseOutcome,
    CostLatency,
    EvaluationRun,
    ImplementationVersions,
    Measurement,
    run_for,
)
from intelligence.model_gateway import ModelGateway
from intelligence.skills.models import CapabilitySnapshot, SkillVersionRef
from intelligence.skills.resolver import resolve
from intelligence.trust.injection import assess_segment
from intelligence.trust.labelling import LabelledSegment

#: The metric names these functions report. They are the configuration's names
#: (`tests/evaluation/thresholds.yaml`), so a rename here without the configuration is a failing test.
METRIC_ROUTE_DETERMINISM = "route_determinism"
METRIC_TOOL_PROPOSAL_VALIDITY = "tool_proposal_schema_validity"
METRIC_UNSUPPORTED_CLAIM_RATE = "unsupported_claim_rate"
METRIC_RETRIEVAL_RECALL = "retrieval_recall_at_10"
METRIC_CROSS_TENANT = "cross_tenant_retrieval"
METRIC_DELETED_AFTER_REFRESH = "deleted_memory_retrieval_after_refresh"
METRIC_INJECTION_UNAUTHORIZED = "injection_unauthorized_effect_executions"
METRIC_INJECTION_DETECTION = "injection_escalation_detection_rate"
METRIC_PROTECTED_REGRESSIONS = "protected_recovery_safety_regressions"
#: Reported but not thresholded by DOSSIER §21.3: the gate reports it as ungated rather than inventing a
#: bar for it, because an unrequired threshold is a second authority for what "good" means.
METRIC_SKILL_RESOLUTION = "skill_resolution_accuracy"


@dataclass(frozen=True, slots=True)
class MetricResult:
    """One metric: what it measured, and how every case it measured fared."""

    measurement: Measurement
    outcomes: tuple[CaseOutcome, ...] = ()


class GroundingVerifier(Protocol):
    """An independent verifier: whether the cited evidence supports the claim (RUN-008's seam)."""

    def supports(self, *, claim: str, citations: Sequence[str]) -> bool: ...


class ToolProposalChecker(Protocol):
    """The tool registry's own schema check: whether a proposed call is valid (RUN-011, Rust)."""

    def is_valid(self, *, tool: str, args: Mapping[str, object]) -> bool: ...


class KnowledgeTextPort(Protocol):
    """The text of a knowledge entry, read from the authoritative source plane (INT-006's seam)."""

    def text_for(self, entry_id: str) -> str | None: ...


@dataclass(frozen=True, slots=True)
class CorpusSample:
    """One pinned injection sample: its identity, its source, its text and the rules it must fire."""

    sample_id: str
    source: str
    text: str
    rules: tuple[str, ...] = ()

    @classmethod
    def from_mapping(cls, data: Mapping[str, object]) -> CorpusSample:
        rules = data.get("rules", [])
        return cls(
            sample_id=str(data.get("id", "")),
            source=str(data.get("source", "")),
            text=str(data.get("text", "")),
            rules=tuple(str(rule) for rule in rules) if isinstance(rules, list) else (),
        )


def load_corpus(path: Path) -> tuple[tuple[CorpusSample, ...], tuple[CorpusSample, ...]]:
    """Read the pinned corpus as (malicious, benign) samples."""
    decoded = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(decoded, Mapping):
        raise ValueError(f"{path} is not a corpus object")
    raw_malicious = decoded.get("malicious", [])
    raw_benign = decoded.get("benign", [])
    if not isinstance(raw_malicious, list) or not isinstance(raw_benign, list):
        raise ValueError(f"{path} carries no malicious/benign lists")
    return (
        tuple(CorpusSample.from_mapping(item) for item in raw_malicious if isinstance(item, Mapping)),
        tuple(CorpusSample.from_mapping(item) for item in raw_benign if isinstance(item, Mapping)),
    )


def run_from_results(
    run_id: str,
    datasets: Sequence[Dataset],
    results: Sequence[MetricResult],
    *,
    versions: ImplementationVersions | None = None,
    cost: CostLatency | None = None,
    started_at: str = "",
    finished_at: str = "",
) -> EvaluationRun:
    """Assemble a run record from the metric results: their measurements and their per-case outcomes."""
    return run_for(
        run_id,
        datasets,
        measurements=[result.measurement for result in results],
        versions=versions,
        cost=cost,
        started_at=started_at,
        finished_at=finished_at,
        outcomes=[outcome for result in results for outcome in result.outcomes],
    )


def _outcome(dataset: Dataset, case: DatasetCase, *, passed: bool) -> CaseOutcome:
    return CaseOutcome(
        dataset_id=dataset.dataset_id,
        case_id=case.case_id,
        protected=case.protected,
        passed=passed,
    )


# ------------------------------------------------------------------- route quality


def _as_int(value: object, *, field: str) -> int:
    """A pinned number, refused rather than coerced when the dataset does not carry one."""
    if isinstance(value, bool) or not isinstance(value, (int, str)):
        raise ValueError(f"{field} is not an integer in the pinned dataset")
    try:
        return int(value)
    except ValueError as error:
        raise ValueError(f"{field} is not an integer in the pinned dataset") from error


def _call_for(case: DatasetCase) -> intelligence_pb2.ModelCallRequest:
    """The pinned request a route-quality case describes."""
    request = case.payload.get("request")
    if not isinstance(request, Mapping):
        raise ValueError(f"case {case.case_id!r} pins no request")
    messages = _as_int(request.get("messages", 0), field="request.messages")
    tools = request.get("tools", [])
    return intelligence_pb2.ModelCallRequest(
        schema_version="v1",
        call_id=f"eval_{case.case_id}",
        route_hint=str(request.get("route_hint", "")),
        messages=[
            intelligence_pb2.RenderedMessage(
                schema_version="v1",
                role=intelligence_pb2.RenderedMessage.ROLE_USER,
                trust_level="trusted_user",
                content_json=json.dumps(f"pinned evaluation input {index}"),
            )
            for index in range(messages)
        ],
        tools=[str(tool) for tool in tools] if isinstance(tools, list) else [],
        max_output_tokens=_as_int(request.get("max_output_tokens", 0), field="max_output_tokens"),
    )


def _class_name(value: int) -> str:
    return intelligence_pb2.ModelRoute.RequestClass.Name(value).removeprefix("REQUEST_CLASS_").lower()


def _route_case_passes(gateway: ModelGateway, case: DatasetCase) -> tuple[bool, str]:
    """Whether one pinned request resolves deterministically to what the case pins."""
    request = _call_for(case)
    try:
        first = gateway.resolve_route(request)
        second = gateway.resolve_route(request)
    except Exception as error:  # a request whose route cannot resolve has not held
        return False, f"did not resolve: {error}"
    if first.route_id != second.route_id:
        return False, "route id changed between identical calls"
    expected_class = case.payload.get("expected_class")
    if expected_class and _class_name(first.route.request_class) != str(expected_class):
        return False, f"resolved {_class_name(first.route.request_class)}, expected {expected_class}"
    expected_rule = case.payload.get("expected_rule")
    if expected_rule and first.route.chosen_by != str(expected_rule):
        return False, f"decided by {first.route.chosen_by}, expected {expected_rule}"
    return True, ""


def measure_route_quality(gateway: ModelGateway, dataset: Dataset) -> MetricResult:
    """The share of pinned requests that resolve deterministically and to their pinned class."""
    outcomes: list[CaseOutcome] = []
    failures: list[str] = []
    for case in dataset.cases:
        passed, detail = _route_case_passes(gateway, case)
        outcomes.append(_outcome(dataset, case, passed=passed))
        if not passed:
            failures.append(f"{case.case_id}: {detail}")
    held = sum(1 for item in outcomes if item.passed)
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_ROUTE_DETERMINISM,
            value=held / len(dataset.cases),
            unit="ratio",
            cases=len(dataset.cases),
            notes="; ".join(failures[:3]),
        ),
        outcomes=tuple(outcomes),
    )


# --------------------------------------------------------------- skill resolution


def _candidates_for(case: DatasetCase) -> tuple[SkillVersionRef, ...]:
    states = case.payload.get("candidate_states", [])
    if not isinstance(states, list):
        raise ValueError(f"case {case.case_id!r} pins no candidate states")
    needs_tool = case.payload.get("needs_tool")
    built: list[SkillVersionRef] = []
    for state in states:
        manifest: dict[str, object] = {}
        if needs_tool and state == "active":
            manifest["tool_needs"] = [str(needs_tool)]
        built.append(
            SkillVersionRef.build(
                skill_id=f"skill_{state}",
                version_id=f"ver_{state}",
                skill_name=f"skill_{state}",
                semver="1.0.0",
                status=str(state),
                manifest=manifest,
            )
        )
    return tuple(built)


def _resolution_case_passes(case: DatasetCase) -> bool:
    candidates = _candidates_for(case)
    task_text = str(case.payload.get("task_text", "") or " ".join(item.skill_id for item in candidates))
    caller_tools = case.payload.get("caller_tools", [])
    caller_capabilities = case.payload.get("caller_capabilities", [])
    snapshot = CapabilitySnapshot(
        tool_names=(
            frozenset(str(tool) for tool in caller_tools) if isinstance(caller_tools, list) else frozenset()
        ),
        capability_needs=(
            frozenset(str(need) for need in caller_capabilities)
            if isinstance(caller_capabilities, list)
            else frozenset()
        ),
    )
    resolution = resolve(
        task_text=task_text,
        snapshot=snapshot,
        candidates=candidates,
        max_skills=len(candidates),
    )
    resolved = sorted(item.skill_id for item in resolution.resolved)
    expected = case.payload.get("expected_resolved", [])
    if not isinstance(expected, list):
        raise ValueError(f"case {case.case_id!r} pins no expected resolution")
    return resolved == sorted(str(item) for item in expected)


def measure_skill_resolution(dataset: Dataset) -> MetricResult:
    """The share of pinned cases whose resolution matches what the dataset expects.

    The task text is built from the candidate identities unless the case pins one, so each case measures
    the property it names (production state, or authority) rather than failing relevance instead.
    """
    outcomes = tuple(_outcome(dataset, case, passed=_resolution_case_passes(case)) for case in dataset.cases)
    held = sum(1 for item in outcomes if item.passed)
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_SKILL_RESOLUTION,
            value=held / len(dataset.cases),
            unit="ratio",
            cases=len(dataset.cases),
        ),
        outcomes=outcomes,
    )


# ------------------------------------------------------------------ injection corpus


def _assess(sample: CorpusSample) -> tuple[bool, bool]:
    """(suspected, renders as data only) for one sample."""
    segment = LabelledSegment.build(segment_id=sample.sample_id, source=sample.source, text=sample.text)
    assessment = assess_segment(segment)
    return assessment.suspected, segment.trust_level.is_data_only


def measure_injection_corpus(
    malicious: Sequence[CorpusSample], benign: Sequence[CorpusSample]
) -> tuple[MetricResult, MetricResult]:
    """The two injection metrics, with one outcome per corpus sample.

    The count is the honest precondition an unauthorized effect would need: the text that reaches context
    must be data (inside its typed boundary), never an instruction. A sample whose source the labeller
    does not recognise still fails closed to untrusted, which is what makes the zero meaningful rather
    than accidental.
    """
    detected = 0
    travelled_as_instruction = 0
    outcomes: list[CaseOutcome] = []
    for sample in malicious:
        suspected, data_only = _assess(sample)
        if suspected:
            detected += 1
        if not data_only:
            travelled_as_instruction += 1
        outcomes.append(
            CaseOutcome(
                dataset_id="injection_corpus",
                case_id=sample.sample_id,
                protected=True,
                passed=suspected and data_only,
            )
        )
    for sample in benign:
        suspected, _data_only = _assess(sample)
        outcomes.append(
            CaseOutcome(
                dataset_id="injection_corpus",
                case_id=sample.sample_id,
                protected=False,
                passed=not suspected,
            )
        )
    false_positives = sum(1 for item in outcomes if not item.protected and not item.passed)
    return (
        MetricResult(
            measurement=Measurement(
                metric=METRIC_INJECTION_UNAUTHORIZED,
                value=travelled_as_instruction,
                unit="count",
                cases=len(malicious),
                notes="malicious samples that reached context as an instruction rather than as data",
            ),
            outcomes=tuple(outcomes),
        ),
        MetricResult(
            measurement=Measurement(
                metric=METRIC_INJECTION_DETECTION,
                value=(detected / len(malicious)) if malicious else 0.0,
                unit="ratio",
                cases=len(malicious),
                notes=f"{detected}/{len(malicious)} malicious flagged; {false_positives} benign flagged",
            ),
            outcomes=tuple(outcomes),
        ),
    )


# ---------------------------------------------------------------------- retrieval


def measure_retrieval(
    index: EmbeddingIndex,
    *,
    tenant_a: str,
    tenant_b: str,
    index_b: EmbeddingIndex,
    corpus: Sequence[tuple[str, SourceDocument]],
    queries: Sequence[tuple[str, str]],
    deleted_ref: str,
    foreign_ref: str,
) -> MetricResult:
    """recall@10 plus the two counts DOSSIER §21.3 requires to be zero, against a real index.

    The caller supplies the corpus it seeded and the queries with the source each must retrieve, because
    what a retrieval corpus *is* belongs to the caller's plane; this function indexes the corpus, measures
    recall against the pinned expectations, and checks that a query run for one tenant returns no row of
    another tenant's and that a source removed with `delete_source` stops being retrievable.
    """
    for _key, document in corpus:
        index.index_source(document)
    for _key, document in corpus:
        index_b.index_source(
            SourceDocument(
                source_kind=document.source_kind,
                source_ref=foreign_ref,
                text=document.text,
                snapshot=document.snapshot,
            )
        )

    recalled = 0
    outcomes: list[CaseOutcome] = []
    for query, expected_ref in queries:
        hits = index.query(query, limit=10)
        found = expected_ref in {hit.source_ref for hit in hits}
        recalled += 1 if found else 0
        outcomes.append(
            CaseOutcome(dataset_id="retrieval", case_id=f"recall:{query}", protected=False, passed=found)
        )

    foreign_hits = [hit for hit in index.query(queries[0][0], limit=10) if hit.source_ref == foreign_ref]
    outcomes.append(
        CaseOutcome(
            dataset_id="retrieval",
            case_id="cross_tenant_retrieval",
            protected=True,
            passed=not foreign_hits,
        )
    )
    index.delete_source(source_kind=SOURCE_KIND_ARTIFACT, source_ref=deleted_ref)
    deleted_hits = [hit for hit in index.query(queries[0][0], limit=10) if hit.source_ref == deleted_ref]
    outcomes.append(
        CaseOutcome(
            dataset_id="retrieval",
            case_id="deleted_source_after_refresh",
            protected=True,
            passed=not deleted_hits,
        )
    )
    del tenant_a, tenant_b

    recall = recalled / len(queries)
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_RETRIEVAL_RECALL,
            value=recall,
            unit="ratio",
            cases=len(queries),
        ),
        outcomes=tuple(outcomes),
    )


# ---------------------------------------------------------------- port-driven metrics


def measure_grounding(dataset: Dataset, verifier: GroundingVerifier | None) -> MetricResult | None:
    """The unsupported-claim rate, or None when no independent verifier is installed.

    The harness does not decide whether evidence supports a claim; that is the verifier's judgement
    (RUN-008's seam). An absent verifier means the metric is *not measured*, and the gate fails closed on
    an unmeasured thresholded metric rather than reading a default.
    """
    if verifier is None:
        return None
    unsupported = 0
    outcomes: list[CaseOutcome] = []
    for case in dataset.cases:
        citations = case.payload.get("citations", [])
        verdict = verifier.supports(
            claim=str(case.payload.get("claim", "")),
            citations=[str(item) for item in citations] if isinstance(citations, list) else [],
        )
        if not verdict:
            unsupported += 1
        outcomes.append(_outcome(dataset, case, passed=verdict == bool(case.payload.get("supported", False))))
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_UNSUPPORTED_CLAIM_RATE,
            value=unsupported / len(dataset.cases),
            unit="ratio",
            cases=len(dataset.cases),
        ),
        outcomes=tuple(outcomes),
    )


def measure_tool_proposals(dataset: Dataset, checker: ToolProposalChecker | None) -> MetricResult | None:
    """The share of proposals the registry's own schema check accepts, or None when none is installed.

    Validating a proposal against a tool declaration is the registry's job (RUN-011, in Rust); a Python
    re-implementation of that schema subset would be a second, drifting authority.
    """
    if checker is None:
        return None
    valid = 0
    outcomes: list[CaseOutcome] = []
    for case in dataset.cases:
        args = case.payload.get("args", {})
        verdict = checker.is_valid(
            tool=str(case.payload.get("tool", "")), args=args if isinstance(args, Mapping) else {}
        )
        if verdict:
            valid += 1
        outcomes.append(_outcome(dataset, case, passed=verdict == bool(case.payload.get("valid", False))))
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_TOOL_PROPOSAL_VALIDITY,
            value=valid / len(dataset.cases),
            unit="ratio",
            cases=len(dataset.cases),
        ),
        outcomes=tuple(outcomes),
    )


# ------------------------------------------------------------------- regressions


def protected_regressions(baseline: Sequence[CaseOutcome], candidate: Sequence[CaseOutcome]) -> Measurement:
    """The protected cases that passed before and fail now — DOSSIER §21.3's blocking row.

    This is why a run records per-case outcomes: the row is a statement about individual protected cases
    across two runs, and an average cannot express it. A case present in the candidate but absent from the
    baseline is not a regression (there was nothing to regress from), and a case that failed in the
    baseline is not one either; both are reported in the notes rather than counted, so the number means
    exactly one thing.
    """
    before = {(item.dataset_id, item.case_id): item for item in baseline if item.protected}
    after = {(item.dataset_id, item.case_id): item for item in candidate if item.protected}
    regressed = [
        key for key, item in sorted(after.items()) if key in before and before[key].passed and not item.passed
    ]
    new_cases = sorted(key for key in after if key not in before)
    return Measurement(
        metric=METRIC_PROTECTED_REGRESSIONS,
        value=len(regressed),
        unit="count",
        cases=len(after),
        notes="; ".join(
            [
                *(f"regressed: {dataset_id}/{case_id}" for dataset_id, case_id in regressed[:3]),
                *(f"new protected case: {dataset_id}/{case_id}" for dataset_id, case_id in new_cases[:3]),
            ]
        ),
    )
