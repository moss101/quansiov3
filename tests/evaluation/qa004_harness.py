"""QA-004 campaign harness: drive the shipped owners over the pinned sets and gate on §21.3.

Nothing here decides quality. Each leg drives the product's own code — the model gateway, the context
plane, the derived index and memory fabric, the skill resolver and evolution gate, the Tool Registry's
argument check — over a pinned, digest-checked set, and turns what it observed into INT-010
`Measurement`s and per-case `CaseOutcome`s. `evaluate_gate` then applies DOSSIER §21.3 as frozen in
`tests/evaluation/thresholds.yaml`.

Two instruments are calibrated before they are trusted:

* the **grounding judge** (a live model, independent of the answering call in role, prompt and wire)
  must agree with `grounding_calibration.v1` at least `judge_calibration_min_agreement`; otherwise
  `unsupported_claim_rate` is reported unmeasured and the gate fails closed;
* the **Tool Registry check** (`quansio-tool-check`, the Rust authority) must agree with every label in
  `proposal_calibration.v1`.

The gated rates are then taken over what the live model *produced* — cited claims answering the pinned
research questions, and tool calls proposed for the pinned tasks — never over the labelled sets.

Environment (read only here, never logged):

* `QUANSIO_TEST_OPENAI_COMPATIBLE_{BASE_URL,API_KEY,MODEL_ID}` — the answering / proposing model;
* `QUANSIO_TEST_ANTHROPIC_API_KEY` (+ optional `QUANSIO_TEST_ANTHROPIC_{BASE_URL,MODEL_ID}`) — the judge;
* `QUANSIO_TEST_OPENAI_API_KEY` — the configured embedding route, for recall with a real embedder;
* `QUANSIO_TEST_POSTGRES_URL` — superuser DSN for scratch databases (memory isolation, derived table);
* `QUANSIO_TOOL_CHECK_BIN` — optional path to `quansio-tool-check` (default: `target/debug`, built on
  demand with cargo).

An absent boundary is reported by name as `BLOCKED_EXTERNAL`; its metric stays unmeasured.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import time
from collections.abc import Callable, Iterator, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml
from intelligence.context.turn import build_turn_context
from intelligence.embeddings.index import (
    INDEX_DIMENSIONS,
    EmbeddingIndex,
    SourceDocument,
)
from intelligence.embeddings.sources import SOURCE_KIND_ARTIFACT, IndexSourceDeletion
from intelligence.evaluation.datasets import Dataset, DatasetCase, load_datasets
from intelligence.evaluation.gate import GateVerdict, ThresholdSet, evaluate_gate
from intelligence.evaluation.metrics import (
    METRIC_CROSS_TENANT,
    METRIC_DELETED_AFTER_REFRESH,
    METRIC_RETRIEVAL_RECALL,
    METRIC_TOOL_PROPOSAL_VALIDITY,
    METRIC_UNSUPPORTED_CLAIM_RATE,
    MetricResult,
    measure_grounding,
    measure_tool_proposals,
)
from intelligence.evaluation.runs import (
    CaseOutcome,
    CostLatency,
    EvaluationRun,
    ImplementationVersions,
    Measurement,
    run_for,
)
from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.catalog import ModelCatalog, ProviderKind
from intelligence.model_gateway.conformance import (
    catalog_model_for_kind,
    live_catalog_for_kind,
)
from intelligence.model_gateway.credentials import env_var_for_handle
from intelligence.model_gateway.errors import GatewayError
from intelligence.model_gateway.tooling import RegistryToolSchemaSource
from intelligence.skills.evolution.evaluate import (
    default_thresholds,
    evaluate_candidate,
    pack_suite,
)
from intelligence.skills.models import SkillVersionRef
from quansio.v1.intelligence import intelligence_pb2

ROOT = Path(__file__).resolve().parents[2]
CAMPAIGN_DIR = ROOT / "tests" / "evaluation" / "campaign"
DATASETS_DIR = ROOT / "tests" / "evaluation" / "datasets"
THRESHOLDS = ROOT / "tests" / "evaluation" / "thresholds.yaml"
CAMPAIGN_CONFIG = CAMPAIGN_DIR / "campaign.yaml"
BASELINE = ROOT / "tests" / "evaluation" / "baselines" / "qa004.v1.json"
SLOS = ROOT / "tests" / "performance" / "slos.yaml"
PACKS = ROOT / "packs" / "skills"

#: Metrics this campaign reports beyond §21.3's rows. The gate lists them as ungated; the campaign's
#: own acceptance reads them (calibration floors, context conformance, skill regressions, latency).
METRIC_CONTEXT_CONFORMANCE = "context_budget_conformance"
METRIC_SKILL_REGRESSION = "skill_regression_pass_rate"
METRIC_JUDGE_AGREEMENT = "grounding_judge_agreement"
METRIC_CHECKER_AGREEMENT = "tool_check_agreement"
METRIC_INDEX_P95 = "index_query_p95_ms"
METRIC_CONTEXT_TOKEN_RATIO = "context_provider_token_ratio"

TENANT_A = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA4AA"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA4BB"
WORKSPACE_A = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA4AA"
WORKSPACE_B = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA4BB"
SUBJECT_A = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA4AA"
SUBJECT_B = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA4BB"
NOW = "2026-09-25T12:00:00Z"
SNAPSHOT = "qa004-campaign-v1"
HASHED_EMBEDDING_ROUTE = "qa004-hashed-embedder"

OPENAI_COMPATIBLE_VARIABLES = (
    "QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL",
    "QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY",
    "QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID",
)
OPENAI_COMPATIBLE_HANDLE = "provider/openai-compatible"


# ============================================================================ pinned inputs


def campaign_sets() -> dict[str, Dataset]:
    """Every pinned campaign set, digest-checked on load."""
    return {dataset.dataset_id: dataset for dataset in load_datasets(CAMPAIGN_DIR)}


def harness_set(dataset_id: str) -> Dataset:
    """A pinned INT-010 dataset (`tests/evaluation/datasets/`)."""
    return next(
        item for item in load_datasets(DATASETS_DIR) if item.dataset_id == dataset_id
    )


def campaign_config() -> dict[str, Any]:
    return dict(yaml.safe_load(CAMPAIGN_CONFIG.read_text(encoding="utf-8")))


def index_query_slo_ms() -> float:
    """DOSSIER §21.2 `Index query ≤ 300 ms`, read from the SLO file QA-009 keeps in sync with it."""
    slos = yaml.safe_load(SLOS.read_text(encoding="utf-8"))["slos"]
    return float(slos["index_query_p95_ms"]["target"])


def corpus_documents(dataset: Dataset) -> dict[str, str]:
    return {
        str(case.payload["ref"]): str(case.payload["text"])
        for case in dataset.tagged("document")
    }


def corpus_queries(dataset: Dataset) -> list[tuple[str, tuple[str, ...]]]:
    return [
        (
            str(case.payload["query"]),
            tuple(str(ref) for ref in case.payload["expected_sources"]),
        )
        for case in dataset.tagged("query")
    ]


def p95(samples: Sequence[float]) -> float:
    if not samples:
        return math.inf
    ordered = sorted(samples)
    return ordered[min(len(ordered) - 1, math.ceil(0.95 * len(ordered)) - 1)]


# ============================================================================ embedders


@dataclass
class HashedEmbedder:
    """A deterministic embedder: hashed bag of words. Identical text embeds identically.

    Used where the property under test is storage and scoping (isolation, deletion, latency), where an
    exact-duplicate foreign row is the *strongest* adversary. It is never release evidence for recall.
    """

    dimensions: int = INDEX_DIMENSIONS

    def embed(
        self, texts: Sequence[str], *, deadline_ms: int
    ) -> tuple[tuple[float, ...], ...]:
        vectors = []
        for text in texts:
            buckets = [0.0] * self.dimensions
            for word in re.findall(r"[a-z0-9]+", text.lower()):
                digest = hashlib.sha256(word.encode("utf-8")).digest()
                buckets[int.from_bytes(digest[:8], "big") % self.dimensions] += 1.0
            norm = math.sqrt(sum(value * value for value in buckets)) or 1.0
            vectors.append(tuple(value / norm for value in buckets))
        return tuple(vectors)


def live_embedder(environ: Mapping[str, str] | None = None) -> tuple[Any, str] | str:
    """The configured embedding route (`config/models.yaml`) with a live credential, or what is missing."""
    from intelligence.embeddings.gateway_provider import gateway_embedder

    source = os.environ if environ is None else environ
    key = source.get("QUANSIO_TEST_OPENAI_API_KEY", "").strip()
    if not key:
        return "QUANSIO_TEST_OPENAI_API_KEY"
    catalog = ModelCatalog.load()
    gateway = ModelGateway(
        catalog=catalog, environ={env_var_for_handle("provider/openai"): key}
    )
    route = gateway.embedding_routes()[0]
    return gateway_embedder(gateway, dimensions=INDEX_DIMENSIONS), route


# ============================================================================ live models


@dataclass
class CallLedger:
    """Every live model call the campaign makes: counted, costed and timed, and bounded."""

    max_calls: int
    max_cost_minor_units: int
    calls: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    cost_minor_units: int = 0
    latencies_ms: list[int] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)
    started: float = field(default_factory=time.monotonic)

    def admit(self) -> None:
        if self.calls >= self.max_calls:
            raise RuntimeError(
                f"campaign call budget exhausted ({self.max_calls} calls)"
            )
        if self.cost_minor_units > self.max_cost_minor_units:
            raise RuntimeError(
                f"campaign cost budget exhausted ({self.max_cost_minor_units} minor units)"
            )
        self.calls += 1

    def record(self, outcome: Any) -> None:
        if outcome is None:
            return
        self.input_tokens += outcome.usage.input_tokens
        self.output_tokens += outcome.usage.output_tokens
        self.cost_minor_units += outcome.cost_estimate_minor_units
        self.latencies_ms.append(outcome.latency_ms)
        if outcome.error_code:
            self.errors.append(outcome.error_code)

    def cost(self, slowest_index_query_ms: float = 0.0) -> CostLatency:
        return CostLatency(
            wall_time_ms=int((time.monotonic() - self.started) * 1000),
            model_calls=self.calls,
            input_tokens=self.input_tokens,
            output_tokens=self.output_tokens,
            cost_minor_units=self.cost_minor_units,
            slowest_index_query_ms=int(slowest_index_query_ms),
        )


@dataclass(frozen=True)
class LiveModel:
    """One live model boundary: the catalog it routes through and its custody environment."""

    kind: ProviderKind
    catalog: ModelCatalog
    environ: Mapping[str, str]
    wire_model: str

    @property
    def model_id(self) -> str:
        return catalog_model_for_kind(self.catalog, self.kind)

    def gateway(self) -> ModelGateway:
        return ModelGateway(
            catalog=self.catalog,
            environ=self.environ,
            tool_schemas=RegistryToolSchemaSource.load(),
        )


def live_model(
    kind: ProviderKind, environ: Mapping[str, str] | None = None
) -> LiveModel | str:
    """The live boundary for `kind`, or the names of the variables it is missing."""
    source = os.environ if environ is None else environ
    if kind is ProviderKind.OPENAI_COMPATIBLE:
        missing = [
            name
            for name in OPENAI_COMPATIBLE_VARIABLES
            if not source.get(name, "").strip()
        ]
        if missing:
            return ", ".join(missing)
        base_url, key, wire = (
            source[name].strip() for name in OPENAI_COMPATIBLE_VARIABLES
        )
        catalog = live_catalog_for_kind(kind, wire_model_id=wire)
        provider = catalog.provider_for(
            catalog.model(catalog_model_for_kind(catalog, kind))
        )
        assert provider.base_url_env and provider.credential_handle_env
        return LiveModel(
            kind=kind,
            catalog=catalog,
            environ={
                provider.base_url_env: base_url,
                provider.credential_handle_env: OPENAI_COMPATIBLE_HANDLE,
                env_var_for_handle(OPENAI_COMPATIBLE_HANDLE): key,
            },
            wire_model=wire,
        )
    if kind is ProviderKind.ANTHROPIC:
        key = source.get("QUANSIO_TEST_ANTHROPIC_API_KEY", "").strip()
        if not key:
            return "QUANSIO_TEST_ANTHROPIC_API_KEY"
        wire = source.get("QUANSIO_TEST_ANTHROPIC_MODEL_ID", "").strip()
        catalog = live_catalog_for_kind(
            kind,
            base_url=source.get("QUANSIO_TEST_ANTHROPIC_BASE_URL", "").strip() or None,
            wire_model_id=wire or None,
        )
        handle = catalog.provider_for(
            catalog.model(catalog_model_for_kind(catalog, kind))
        ).resolve_credential_handle({})
        return LiveModel(
            kind=kind,
            catalog=catalog,
            environ={env_var_for_handle(handle): key},
            wire_model=wire
            or catalog.model(catalog_model_for_kind(catalog, kind)).model_id,
        )
    raise ValueError(f"no live boundary for {kind}")


def _message(
    role: int, trust: str, content: object, *, stable: bool = False
) -> intelligence_pb2.RenderedMessage:
    return intelligence_pb2.RenderedMessage(
        schema_version="v1",
        role=role,
        trust_level=trust,
        content_json=json.dumps(content),
        cache_hint="stable" if stable else "",
    )


def _request(
    model: LiveModel,
    messages: Sequence[intelligence_pb2.RenderedMessage],
    *,
    call_id: str,
    tools: Sequence[str] = (),
    max_output_tokens: int,
    timeout_ms: int,
) -> intelligence_pb2.ModelCallRequest:
    return intelligence_pb2.ModelCallRequest(
        schema_version="v1",
        call_id=call_id,
        run_id="run_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        turn_id="trn_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        route_hint=model.model_id,
        capability_projection_id="cp_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        context_projection_id="ctx_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        messages=list(messages),
        tools=list(tools),
        max_output_tokens=max_output_tokens,
        stream=True,
        timeout_ms=timeout_ms,
    )


@dataclass(frozen=True)
class ModelReply:
    text: str
    tool_calls: tuple[tuple[str, str], ...]
    input_tokens: int
    error: str | None


def call_model(
    gateway: ModelGateway,
    request: intelligence_pb2.ModelCallRequest,
    ledger: CallLedger,
) -> ModelReply:
    """One live call through the gateway; text, proposed tool calls and the provider's usage.

    The request's own `timeout_ms` bounds the call. A call the gateway refuses or fails is recorded in
    the ledger and returned with its error, so the case that made it fails closed.
    """
    ledger.admit()
    text: list[str] = []
    calls: list[tuple[str, str]] = []
    error: str | None = None
    outcome = None
    try:
        fulfillment = gateway.fulfill(request)
        try:
            for event in fulfillment:
                if event.kind == intelligence_pb2.ModelEvent.KIND_DELTA:
                    text.append(event.text_delta)
                elif event.kind == intelligence_pb2.ModelEvent.KIND_TOOL_CALL:
                    calls.append((event.tool_name, event.tool_args_json))
                elif event.kind == intelligence_pb2.ModelEvent.KIND_ERROR:
                    error = event.error.code if event.HasField("error") else "error"
        finally:
            outcome = fulfillment.outcome
    except GatewayError as failure:
        error = f"{failure.code}: {failure.message}"
    ledger.record(outcome)
    if error:
        ledger.errors.append(error)
    return ModelReply(
        text="".join(text),
        tool_calls=tuple(calls),
        input_tokens=outcome.usage.input_tokens if outcome is not None else 0,
        error=error,
    )


def parse_json_object(text: str) -> dict[str, Any] | None:
    """The first JSON object in a model reply (models wrap JSON in prose or fences)."""
    stripped = re.sub(
        r"^```(?:json)?|```$", "", text.strip(), flags=re.MULTILINE
    ).strip()
    for candidate in (stripped, *re.findall(r"\{.*\}", stripped, flags=re.DOTALL)):
        try:
            decoded = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(decoded, dict):
            return decoded
    return None


# ============================================================================ grounding


@dataclass
class ModelGroundingJudge:
    """INT-010's `GroundingVerifier` port, answered by a live model over the cited evidence text.

    A citation that does not resolve to pinned evidence is unsupported without asking the model (the
    deterministic half of grounding); a verdict the model does not state as `{"supported": bool}` counts
    as unsupported, so a malformed judge cannot inflate the score.
    """

    model: LiveModel
    evidence: Mapping[str, str]
    ledger: CallLedger
    timeout_ms: int
    malformed: int = 0
    _gateway: ModelGateway | None = None

    def supports(self, *, claim: str, citations: Sequence[str]) -> bool:
        if not citations or any(ref not in self.evidence for ref in citations):
            return False
        if self._gateway is None:
            self._gateway = self.model.gateway()
        passages = "\n\n".join(f"[{ref}]\n{self.evidence[ref]}" for ref in citations)
        messages = [
            _message(
                intelligence_pb2.RenderedMessage.ROLE_SYSTEM,
                "TRUSTED_SYSTEM",
                "You are a strict fact checker. Decide whether the cited passages, and nothing else, "
                "fully support the claim. A claim that adds, changes or overstates any detail is not "
                'supported. Reply with only a JSON object: {"supported": true} or {"supported": false}.',
                stable=True,
            ),
            _message(
                intelligence_pb2.RenderedMessage.ROLE_USER,
                "TRUSTED_USER",
                f"Passages:\n{passages}\n\nClaim: {claim}",
            ),
        ]
        reply = call_model(
            self._gateway,
            _request(
                self.model,
                messages,
                call_id=f"call_judge_{hashlib.sha256(claim.encode()).hexdigest()[:12]}",
                max_output_tokens=512,
                timeout_ms=self.timeout_ms,
            ),
            self.ledger,
        )
        verdict = parse_json_object(reply.text)
        if verdict is None or not isinstance(verdict.get("supported"), bool):
            self.malformed += 1
            return False
        return bool(verdict["supported"])


@dataclass(frozen=True)
class GeneratedClaim:
    question_id: str
    text: str
    citations: tuple[str, ...]


def answer_with_citations(
    model: LiveModel,
    gateway: ModelGateway,
    case: DatasetCase,
    evidence: Mapping[str, str],
    ledger: CallLedger,
    *,
    timeout_ms: int,
    max_output_tokens: int,
) -> tuple[tuple[GeneratedClaim, ...], int, int]:
    """Answer one pinned research question through the context plane; (claims, estimated, provider tokens).

    The runtime's side of a turn is played honestly: instructions and the question are canonical content,
    the evidence documents are `knowledge` segments, and `build_turn_context` labels, packs and renders
    them. An answer that is not the requested JSON yields no claims and is counted by the caller as one
    unsupported claim (fail closed).
    """
    refs = [str(ref) for ref in case.payload["evidence"]]
    segments = [
        {
            "seq": 0,
            "source": {"kind": "system", "ref": "qa004_research"},
            "role": "system",
            "content": "You are a research teammate. Answer only from the knowledge passages provided in "
            "this conversation. Every claim must cite the passage ids that support it.",
            "snapshot_ref": SNAPSHOT,
            "required": True,
        },
        {
            "seq": 1,
            "source": {"kind": "policy", "ref": "qa004_format"},
            "role": "operator",
            "content": 'Reply with only a JSON object {"claims": [{"text": "...", "citations": ["<id>"]}]} '
            "holding two to four short factual claims. Do not state anything the passages do not say.",
            "snapshot_ref": SNAPSHOT,
            "required": True,
        },
    ]
    for offset, ref in enumerate(refs, start=2):
        segments.append(
            {
                "seq": offset,
                "source": {"kind": "knowledge", "ref": ref},
                "role": "user",
                "content": f"Knowledge passage id {ref}:\n{evidence[ref]}",
                "snapshot_ref": SNAPSHOT,
                "required": True,
            }
        )
    segments.append(
        {
            "seq": len(segments),
            "source": {"kind": "thread", "ref": f"msg_{case.case_id}"},
            "role": "user",
            "content": str(case.payload["question"]),
            "snapshot_ref": SNAPSHOT,
            "required": True,
        }
    )
    context = build_turn_context(
        run_id="run_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        turn_id="trn_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
        snapshot_id=SNAPSHOT,
        token_budget=24_000,
        policy_snapshot_digest="sha256:qa004",
        segment_requests=[json.dumps(segment) for segment in segments],
    )
    reply = call_model(
        gateway,
        _request(
            model,
            context.messages,
            call_id=f"call_answer_{case.case_id}",
            max_output_tokens=max_output_tokens,
            timeout_ms=timeout_ms,
        ),
        ledger,
    )
    decoded = parse_json_object(reply.text) or {}
    claims = []
    for raw in (
        decoded.get("claims", []) if isinstance(decoded.get("claims"), list) else []
    ):
        if not isinstance(raw, dict) or not str(raw.get("text", "")).strip():
            continue
        cites = raw.get("citations", [])
        claims.append(
            GeneratedClaim(
                question_id=case.case_id,
                text=str(raw["text"]).strip(),
                citations=tuple(str(ref) for ref in cites)
                if isinstance(cites, list)
                else (),
            )
        )
    return tuple(claims), context.projection.token_ledger.used, reply.input_tokens


def measure_unsupported_claims(
    claims_by_question: Mapping[str, Sequence[GeneratedClaim]],
    judge: Any,
    dataset: Dataset,
) -> MetricResult:
    """§21.3 `unsupported_claim_rate` over the claims the live model produced (a no-answer counts once)."""
    unsupported = 0
    total = 0
    outcomes: list[CaseOutcome] = []
    notes: list[str] = []
    for case in dataset.cases:
        claims = claims_by_question.get(case.case_id, ())
        if not claims:
            unsupported += 1
            total += 1
            notes.append(f"{case.case_id}: no parseable cited claims")
            outcomes.append(
                CaseOutcome(
                    dataset_id=dataset.dataset_id,
                    case_id=case.case_id,
                    protected=False,
                    passed=False,
                )
            )
            continue
        held = True
        for claim in claims:
            total += 1
            if not judge.supports(claim=claim.text, citations=list(claim.citations)):
                unsupported += 1
                held = False
                notes.append(f"{case.case_id}: unsupported: {claim.text[:80]}")
        outcomes.append(
            CaseOutcome(
                dataset_id=dataset.dataset_id,
                case_id=case.case_id,
                protected=False,
                passed=held,
            )
        )
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_UNSUPPORTED_CLAIM_RATE,
            value=unsupported / total,
            unit="ratio",
            cases=total,
            notes="; ".join(notes[:4]),
        ),
        outcomes=tuple(outcomes),
    )


def calibration_agreement(
    metric: str, result: MetricResult | None, dataset: Dataset
) -> Measurement:
    """How often an instrument agreed with a labelled set (INT-010's per-case outcomes)."""
    if result is None:
        return Measurement(
            metric=metric,
            value=0.0,
            unit="ratio",
            cases=len(dataset.cases),
            notes="not run",
        )
    agreed = sum(1 for outcome in result.outcomes if outcome.passed)
    missed = [outcome.case_id for outcome in result.outcomes if not outcome.passed]
    return Measurement(
        metric=metric,
        value=agreed / len(result.outcomes),
        unit="ratio",
        cases=len(result.outcomes),
        notes=("disagreed: " + ", ".join(missed[:6])) if missed else "",
    )


# ============================================================================ tool proposals


class RegistryToolCheck:
    """INT-010's `ToolProposalChecker` port, answered by the Rust Tool Registry (`quansio-tool-check`)."""

    def __init__(self, binary: Path | None = None) -> None:
        self.binary = binary or tool_check_binary()

    def verdicts(self, proposals: Sequence[tuple[str, object]]) -> list[dict[str, Any]]:
        lines = "".join(
            json.dumps({"tool": tool, "args": args}) + "\n" for tool, args in proposals
        )
        completed = subprocess.run(
            [str(self.binary)],
            input=lines,
            capture_output=True,
            text=True,
            check=True,
            timeout=60,
        )
        verdicts = [
            json.loads(line) for line in completed.stdout.splitlines() if line.strip()
        ]
        if len(verdicts) != len(proposals):
            raise RuntimeError(
                f"quansio-tool-check answered {len(verdicts)} of {len(proposals)} proposals"
            )
        return verdicts

    def is_valid(self, *, tool: str, args: Mapping[str, object]) -> bool:
        return bool(self.verdicts([(tool, dict(args))])[0]["valid"])


def tool_check_binary() -> Path:
    """`QUANSIO_TOOL_CHECK_BIN`, or the workspace build (built with cargo when absent)."""
    configured = os.environ.get("QUANSIO_TOOL_CHECK_BIN", "").strip()
    if configured:
        return Path(configured)
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    binary = target / "debug" / "quansio-tool-check"
    subprocess.run(
        ["cargo", "build", "-q", "-p", "quansio-tools", "--bin", "quansio-tool-check"],
        cwd=ROOT,
        check=True,
        timeout=900,
    )
    return binary


def propose_tool_calls(
    model: LiveModel,
    gateway: ModelGateway,
    case: DatasetCase,
    ledger: CallLedger,
    *,
    timeout_ms: int,
    max_output_tokens: int,
) -> ModelReply:
    """Offer the pinned task with its real registry tools; return what the model proposed."""
    messages = [
        _message(
            intelligence_pb2.RenderedMessage.ROLE_SYSTEM,
            "TRUSTED_SYSTEM",
            "You are an operations teammate. Carry out the user's request by calling exactly one of the "
            "provided tools with arguments that match its schema. Do not answer in prose.",
            stable=True,
        ),
        _message(
            intelligence_pb2.RenderedMessage.ROLE_USER,
            "TRUSTED_USER",
            str(case.payload["instruction"]),
        ),
    ]
    return call_model(
        gateway,
        _request(
            model,
            messages,
            call_id=f"call_propose_{case.case_id}",
            tools=[str(tool) for tool in case.payload["tools"]],
            max_output_tokens=max_output_tokens,
            timeout_ms=timeout_ms,
        ),
        ledger,
    )


def measure_live_proposals(
    replies: Mapping[str, ModelReply], checker: RegistryToolCheck, dataset: Dataset
) -> MetricResult | None:
    """§21.3 `tool_proposal_schema_validity` over every call the live model proposed.

    A task the model answered without a call proposed nothing, so it is outside the population and is
    reported in the notes; with no proposal at all the metric is unmeasured.
    """
    proposals: list[tuple[str, str, object]] = []
    silent: list[str] = []
    for case in dataset.cases:
        reply = replies.get(case.case_id)
        if reply is None or not reply.tool_calls:
            silent.append(case.case_id)
            continue
        for name, args_json in reply.tool_calls:
            try:
                args: object = json.loads(args_json) if args_json else {}
            except json.JSONDecodeError:
                args = args_json  # not an object: the registry check refuses it
            proposals.append((case.case_id, name, args))
    if not proposals:
        return None
    verdicts = checker.verdicts([(name, args) for _case, name, args in proposals])
    valid = sum(1 for verdict in verdicts if verdict["valid"])
    by_case: dict[str, bool] = {}
    invalid_notes: list[str] = []
    for (case_id, name, _args), verdict in zip(proposals, verdicts, strict=True):
        by_case[case_id] = by_case.get(case_id, True) and bool(verdict["valid"])
        if not verdict["valid"]:
            invalid_notes.append(
                f"{case_id}: {name} {verdict['rule']} {verdict.get('path', '')}"
            )
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_TOOL_PROPOSAL_VALIDITY,
            value=valid / len(proposals),
            unit="ratio",
            cases=len(proposals),
            notes="; ".join(
                [
                    *invalid_notes[:4],
                    *(f"no proposal: {case_id}" for case_id in silent[:4]),
                ]
            ),
        ),
        outcomes=tuple(
            CaseOutcome(
                dataset_id=dataset.dataset_id,
                case_id=case_id,
                protected=False,
                passed=passed,
            )
            for case_id, passed in sorted(by_case.items())
        ),
    )


# ============================================================================ context budget


def evaluate_context_budget(dataset: Dataset) -> MetricResult:
    """Every pinned packing case through `build_turn_context`, checked against what DOMAIN §11.2 requires."""
    from intelligence.context.projection import ContextError

    outcomes: list[CaseOutcome] = []
    failures: list[str] = []
    for case in dataset.cases:
        budget = int(case.payload["token_budget"])
        requests = [dict(item) for item in case.payload["segment_requests"]]
        expect = str(case.payload["expect"])
        problems: list[str] = []
        try:
            context = build_turn_context(
                run_id="run_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
                turn_id="trn_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
                snapshot_id=SNAPSHOT,
                token_budget=budget,
                policy_snapshot_digest="sha256:qa004",
                segment_requests=[json.dumps(item) for item in requests],
            )
        except ContextError as refused:
            if expect != "refused":
                problems.append(f"refused: {refused}")
        else:
            if expect == "refused":
                problems.append("an unpackable request was packed")
            else:
                problems.extend(context_problems(context.projection, requests, budget))
        outcomes.append(
            CaseOutcome(
                dataset_id=dataset.dataset_id,
                case_id=case.case_id,
                protected=case.protected,
                passed=not problems,
            )
        )
        failures.extend(f"{case.case_id}: {problem}" for problem in problems)
    held = sum(1 for outcome in outcomes if outcome.passed)
    return MetricResult(
        measurement=Measurement(
            metric=METRIC_CONTEXT_CONFORMANCE,
            value=held / len(outcomes),
            unit="ratio",
            cases=len(outcomes),
            notes="; ".join(failures[:4]),
        ),
        outcomes=tuple(outcomes),
    )


def context_problems(
    projection: Any, requests: Sequence[Mapping[str, Any]], budget: int
) -> list[str]:
    """What a packed projection violates, if anything."""
    from intelligence.trust.labelling import BOUNDARY_OPEN

    problems: list[str] = []
    ledger = projection.token_ledger
    kept = {segment.seq for segment in projection.segments}
    by_seq = {int(item["seq"]): item for item in requests}
    if ledger.used > budget or ledger.budget != budget:
        problems.append(f"ledger {ledger.used}/{ledger.budget} exceeds budget {budget}")
    if ledger.used != sum(segment.token_estimate for segment in projection.segments):
        problems.append("the ledger does not add up to its segments")
    by_source = json.loads(ledger.by_source_json or "{}")
    if sum(by_source.values()) != ledger.used:
        problems.append("the per-source ledger does not add up")
    for seq, item in by_seq.items():
        if item.get("required") and seq not in kept:
            problems.append(f"required segment {seq} was dropped")
    dropped = [seq for seq in by_seq if seq not in kept]
    if dropped and not projection.degradation_json:
        problems.append("segments were dropped without a degradation record")
    if not dropped and projection.degradation_json:
        problems.append("degradation recorded with nothing dropped")
    recorded = sum(
        int(json.loads(item)["dropped"]) for item in projection.degradation_json
    )
    if recorded != len(dropped):
        problems.append(
            f"degradation records {recorded} drops, {len(dropped)} happened"
        )
    # An assistant turn and the tool results answering it are kept or dropped together.
    conversation = sorted(
        (item for item in requests if item["role"] not in ("system", "operator")),
        key=lambda item: item["seq"],
    )
    for index, item in enumerate(conversation):
        if item["role"] == "tool":
            head = next(
                (
                    prior
                    for prior in reversed(conversation[:index])
                    if prior["role"] != "tool"
                ),
                None,
            )
            if head is not None and (int(head["seq"]) in kept) != (
                int(item["seq"]) in kept
            ):
                problems.append(
                    f"tool result {item['seq']} split from its assistant turn"
                )
    # Optional conversation is dropped oldest first: nothing optional that is kept is older than a drop.
    optional = [int(item["seq"]) for item in conversation if not item.get("required")]
    dropped_optional = [seq for seq in optional if seq not in kept]
    kept_optional = [seq for seq in optional if seq in kept]
    if (
        dropped_optional
        and kept_optional
        and max(dropped_optional) > min(kept_optional)
    ):
        head_kept = min(kept_optional)
        if any(
            seq > head_kept for seq in dropped_optional if by_seq[seq]["role"] != "tool"
        ):
            problems.append(
                "a newer optional turn was dropped while an older one was kept"
            )
    # The stable prefix: system first, then operator instructions, then everything else.
    roles = [message.role for message in projection.messages]
    order = {
        intelligence_pb2.RenderedMessage.ROLE_SYSTEM: 0,
        intelligence_pb2.RenderedMessage.ROLE_OPERATOR: 1,
    }
    ranks = [order.get(role, 2) for role in roles]
    if ranks != sorted(ranks):
        problems.append("instructions are not the stable prefix")
    # Untrusted content travels inside the typed boundary.
    for segment, message in zip(projection.segments, projection.messages, strict=True):
        if (
            segment.trust_level.lower() == "untrusted_external"
            and BOUNDARY_OPEN not in message.content_json
        ):
            problems.append(
                f"untrusted segment {segment.seq} rendered without its boundary"
            )
    return problems


# ============================================================================ skills


def shipped_skill_versions() -> list[SkillVersionRef]:
    """Every built-in pack as the registry would hold its ACTIVE version."""
    versions = []
    for skill_file in sorted(PACKS.glob("*/skill.yaml")):
        meta = yaml.safe_load(
            (skill_file.parent / "meta.yaml").read_text(encoding="utf-8")
        )
        manifest = yaml.safe_load(skill_file.read_text(encoding="utf-8"))
        versions.append(
            SkillVersionRef.build(
                skill_id=str(meta["id"]),
                version_id=f"sklv_{meta['name']}_{meta['semver']}",
                skill_name=str(meta["name"]),
                semver=str(meta["semver"]),
                status=str(meta["status"]),
                manifest=manifest,
                provenance=str(meta.get("provenance", "")),
            )
        )
    return versions


def measure_skill_regression() -> tuple[MetricResult, dict[str, str]]:
    """Each shipped pack's ACTIVE version against its own suite through CAP-008's regression gate."""
    thresholds = default_thresholds()
    outcomes: list[CaseOutcome] = []
    failures: list[str] = []
    versions: dict[str, str] = {}
    for version in shipped_skill_versions():
        versions[version.skill_id] = version.version_id
        if not pack_suite(version.manifest.eval_suite_id):
            failures.append(
                f"{version.skill_name}: its suite {version.manifest.eval_suite_id} is not shipped"
            )
        evaluation = evaluate_candidate(
            version,
            version,
            run_id=f"qa004-skill-{version.skill_name}",
            thresholds=thresholds,
        )
        for outcome in evaluation.run.outcomes:
            outcomes.append(
                CaseOutcome(
                    dataset_id=f"skill_{version.skill_name.replace('-', '_')}",
                    case_id=outcome.case_id,
                    protected=True,
                    passed=outcome.passed,
                )
            )
        if not evaluation.passed:
            failures.append(f"{version.skill_name}: {evaluation.verdict.reason}")
    held = sum(1 for outcome in outcomes if outcome.passed)
    return (
        MetricResult(
            measurement=Measurement(
                metric=METRIC_SKILL_REGRESSION,
                value=held / len(outcomes) if outcomes else 0.0,
                unit="ratio",
                cases=len(outcomes),
                notes="; ".join(failures[:4]),
            ),
            outcomes=tuple(outcomes),
        ),
        versions,
    )


# ============================================================================ retrieval


def measure_corpus_recall(
    index: EmbeddingIndex, dataset: Dataset, *, k: int
) -> tuple[MetricResult, list[float]]:
    """recall@k over the pinned corpus and questions; also the per-query latency samples."""
    documents = corpus_documents(dataset)
    for ref, text in documents.items():
        index.index_source(
            SourceDocument(
                source_kind=SOURCE_KIND_ARTIFACT,
                source_ref=ref,
                text=text,
                snapshot=SNAPSHOT,
            )
        )
    outcomes: list[CaseOutcome] = []
    latencies: list[float] = []
    missed: list[str] = []
    recalled = 0
    for case in dataset.tagged("query"):
        started = time.perf_counter()
        hits = index.query(str(case.payload["query"]), limit=k)
        latencies.append((time.perf_counter() - started) * 1000)
        found = {hit.source_ref for hit in hits}
        expected = {str(ref) for ref in case.payload["expected_sources"]}
        passed = expected <= found
        recalled += 1 if passed else 0
        if not passed:
            missed.append(case.case_id)
        outcomes.append(
            CaseOutcome(
                dataset_id=dataset.dataset_id,
                case_id=case.case_id,
                protected=False,
                passed=passed,
            )
        )
    return (
        MetricResult(
            measurement=Measurement(
                metric=METRIC_RETRIEVAL_RECALL,
                value=recalled / len(outcomes),
                unit="ratio",
                cases=len(outcomes),
                notes=("missed: " + ", ".join(missed[:6])) if missed else "",
            ),
            outcomes=tuple(outcomes),
        ),
        latencies,
    )


# ============================================================================ scratch databases


def seed_scratch_database(admin_url: str, name: str) -> str:
    """A migrated scratch database with both campaign tenants and workspaces; returns its URL."""
    completed = subprocess.run(
        [
            sys.executable,
            str(ROOT / "scripts" / "dev" / "seed_test_database.py"),
            "--admin-url",
            admin_url,
            "--database",
            name,
            f"--tenant={TENANT_A}",
            f"--tenant={TENANT_B}",
            f"--workspace={WORKSPACE_A}",
            f"--workspace={WORKSPACE_B}",
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"seeding {name} failed:\n{completed.stdout}\n{completed.stderr}"
        )
    return str(json.loads(completed.stdout.strip().splitlines()[-1])["url"])


def drop_scratch_database(admin_url: str, name: str) -> None:
    import psycopg

    with psycopg.connect(admin_url, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


@contextlib.contextmanager
def scratch_database(admin_url: str, label: str) -> Iterator[str]:
    name = f"quansio_qa004_{label}_{os.getpid()}"
    url = seed_scratch_database(admin_url, name)
    try:
        yield url
    finally:
        drop_scratch_database(admin_url, name)


def app_role_connect(url: str) -> Callable[[], Any]:
    """Connections that run as `quansio_app`, so forced row-level security applies (no superuser bypass)."""
    import psycopg

    def connect() -> Any:
        connection = psycopg.connect(url, autocommit=True)
        connection.execute("SET ROLE quansio_app")
        connection.autocommit = False
        return connection

    return connect


# ============================================================================ memory isolation


@dataclass
class MemoryHarness:
    """Tenant A and tenant B, each with its own fabric and index, over one database as `quansio_app`."""

    url: str
    embedder: Any
    model_id: str

    def __post_init__(self) -> None:
        from intelligence.embeddings import SqlEmbeddingStore, index_for
        from intelligence.memory.store import SqlMemoryStore, memory_for

        connect = app_role_connect(self.url)
        self.connect = connect
        self.fabric = {
            tenant: memory_for(SqlMemoryStore(connect), tenant_id=tenant)
            for tenant in (TENANT_A, TENANT_B)
        }
        self.index = {
            tenant: index_for(
                SqlEmbeddingStore(connect),
                self.embedder,
                model_id=self.model_id,
                tenant_id=tenant,
                workspace_id=None,
            )
            for tenant in (TENANT_A, TENANT_B)
        }
        self.deletion = IndexSourceDeletion(
            index_for_tenant=lambda tenant: self.index[tenant]
        )

    def reset(self) -> None:
        import psycopg

        with psycopg.connect(self.url, autocommit=True) as connection:
            connection.execute("DELETE FROM memory_entries")
            connection.execute("DELETE FROM derived.embeddings")

    def remember(
        self, tenant: str, content: str, *, expires_at: str = "", promote: bool = True
    ) -> str:
        from intelligence.memory.candidates import MemoryCandidate, propose
        from intelligence.memory.models import MemoryProvenance, MemoryStatus

        fabric = self.fabric[tenant]
        outcome = propose(
            fabric,
            MemoryCandidate(
                subject_ref=SUBJECT_A if tenant == TENANT_A else SUBJECT_B,
                content=content,
                provenance_kind=MemoryProvenance.EXPLICIT_USER,
                provenance_ref="msg_01J8Z3K6F1N8VQ2X5W9Y0QA4AA",
                expires_at=expires_at,
            ),
            workspace_id=WORKSPACE_A if tenant == TENANT_A else WORKSPACE_B,
        )
        if promote:
            fabric.set_status(outcome.entry.id, MemoryStatus.ACTIVE)
        return outcome.entry.id

    def sync(self, tenant: str, now: str = NOW) -> None:
        from intelligence.memory.retrieval import MemoryIndexer

        MemoryIndexer(fabric=self.fabric[tenant], index=self.index[tenant]).synchronize(
            now
        )

    def served(
        self, tenant: str, query: str, now: str = NOW
    ) -> tuple[set[str], set[str]]:
        """(memory ids retrieval served, source refs the raw index answered) for `tenant`'s query."""
        from intelligence.memory.retrieval import retrieve

        served = {
            item.entry.id
            for item in retrieve(
                self.fabric[tenant], self.index[tenant], query, now=now, limit=10
            )
        }
        raw = {hit.source_ref for hit in self.index[tenant].query(query, limit=10)}
        return served, raw

    def ids_of(self, tenant: str) -> set[str]:
        return {entry.id for entry in self.fabric[tenant].entries(limit=200)}


def run_memory_adversarial(
    harness: MemoryHarness, dataset: Dataset, corpus: Dataset
) -> tuple[MetricResult, MetricResult]:
    """The adversarial set: (cross-tenant rows served, deleted memories served), one outcome per case.

    Every scenario carries a **positive control** — the attacked tenant's own memory is served, a
    memory is served before it is deleted, the forged statement succeeds under its own tenant — so a
    zero means the attack was refused, not that the scenario could not have seen it. A failed control
    counts as a leak (fail closed) and is named in the notes.
    """
    from intelligence.memory.retrieval import forget_memory

    foreign_total = 0
    deleted_total = 0
    cross_outcomes: list[CaseOutcome] = []
    deleted_outcomes: list[CaseOutcome] = []
    notes: list[str] = []
    for case in dataset.cases:
        harness.reset()
        payload = case.payload
        scenario = str(payload["scenario"])
        leaks = 0
        controls: list[str] = []

        def control(held: bool, what: str, sink: list[str] = controls) -> None:
            if not held:
                sink.append(what)

        if scenario in (
            "cross_tenant_identical",
            "cross_tenant_only_match",
            "cross_tenant_poisoned",
        ):
            own = harness.remember(TENANT_A, str(payload["own"]))
            harness.remember(TENANT_B, str(payload["foreign"]))
            harness.sync(TENANT_A)
            harness.sync(TENANT_B)
            foreign = harness.ids_of(TENANT_B)
            served, raw = harness.served(TENANT_A, str(payload["query"]))
            leaks = len(served & foreign) + len(raw & foreign)
            control(own in served, "tenant A's own memory is served")
            control(
                bool(harness.served(TENANT_B, str(payload["query"]))[0] & foreign),
                "tenant B's memory answers B",
            )
        elif scenario == "cross_tenant_bulk":
            own = harness.remember(TENANT_A, str(payload["own"]))
            for text in corpus_documents(corpus).values():
                harness.remember(TENANT_B, text)
            harness.sync(TENANT_A)
            harness.sync(TENANT_B)
            foreign = harness.ids_of(TENANT_B)
            answered_b = 0
            for query, _expected in corpus_queries(corpus):
                served, raw = harness.served(TENANT_A, query)
                leaks += len(served & foreign) + len(raw & foreign)
                answered_b += int(bool(harness.served(TENANT_B, query)[0]))
            control(
                own in harness.served(TENANT_A, str(payload["own"]))[0],
                "tenant A's own memory is served",
            )
            control(
                answered_b == len(corpus_queries(corpus)),
                "every question answers tenant B from B's memory",
            )
        elif scenario == "forged_tenant_row":
            own = harness.remember(TENANT_A, "Tenant A's own memory.")
            harness.sync(TENANT_A)
            # The forged row claims tenant A while the session is tenant B's: refused by RLS WITH CHECK.
            leaks += int(forge_embedding(harness, context=TENANT_B, claimed=TENANT_A))
            # The same statement under tenant A's own context succeeds: the refusal above was RLS.
            control(
                forge_embedding(harness, context=TENANT_A, claimed=TENANT_A),
                "the statement is valid for its own tenant",
            )
            # The memory table: tenant B's context cannot touch tenant A's rows.
            leaks += touch_memories(harness, context=TENANT_B, claimed=TENANT_A)
            control(
                touch_memories(
                    harness, context=TENANT_A, claimed=TENANT_A, commit=False
                )
                > 0,
                "tenant A can touch its own rows",
            )
            served, _raw = harness.served(TENANT_A, "Tenant A's own memory")
            control(own in served, "tenant A's memory still answers after the attack")
        elif scenario == "tenantless_read":
            harness.remember(TENANT_A, "Tenant A's memory.")
            harness.remember(TENANT_B, "Tenant B's memory.")
            harness.sync(TENANT_A)
            harness.sync(TENANT_B)
            counts = table_counts(harness, context=None)
            leaks += sum(counts.values())
            own_context = table_counts(harness, context=TENANT_A)
            leaks += sum(
                table_counts(harness, context=TENANT_A, foreign_only=True).values()
            )
            control(
                all(value > 0 for value in own_context.values()),
                "tenant A's context sees its own rows",
            )
        elif scenario == "deleted_after_refresh":
            doomed = harness.remember(TENANT_A, str(payload["own"]))
            kept = harness.remember(TENANT_A, str(payload["keep"]))
            harness.sync(TENANT_A)
            control(
                doomed in harness.served(TENANT_A, str(payload["query"]))[0],
                "the memory is served before it is forgotten",
            )
            forget_memory(harness.fabric[TENANT_A], harness.deletion, doomed)
            harness.sync(TENANT_A)
            served, raw = harness.served(TENANT_A, str(payload["query"]))
            leaks = int(doomed in served) + int(doomed in raw)
            control(
                kept in harness.served(TENANT_A, str(payload["keep"]))[0],
                "the unrelated memory still answers",
            )
        elif scenario == "deleted_before_refresh":
            doomed = harness.remember(TENANT_A, str(payload["own"]))
            harness.sync(TENANT_A)
            control(
                doomed in harness.served(TENANT_A, str(payload["query"]))[0],
                "the memory is served before deletion",
            )
            harness.fabric[TENANT_A].delete(doomed)
            served, raw = harness.served(TENANT_A, str(payload["query"]))
            leaks = int(doomed in served)
            control(
                doomed in raw,
                "the index has not refreshed (the stale row is still there)",
            )
        elif scenario == "expired_after_refresh":
            expired = harness.remember(
                TENANT_A, str(payload["own"]), expires_at=str(payload["expires_at"])
            )
            before = "2025-12-01T00:00:00Z"
            harness.sync(TENANT_A, now=before)
            control(
                expired
                in harness.served(TENANT_A, str(payload["query"]), now=before)[0],
                "the memory is served before it expires",
            )
            harness.sync(TENANT_A)
            served, raw = harness.served(TENANT_A, str(payload["query"]))
            leaks = int(expired in served) + int(expired in raw)
        elif scenario == "candidate_never_served":
            candidate = harness.remember(TENANT_A, str(payload["own"]), promote=False)
            promoted = harness.remember(
                TENANT_A, str(payload["own"]) + " Confirmed by the user."
            )
            harness.sync(TENANT_A)
            served, raw = harness.served(TENANT_A, str(payload["query"]))
            leaks = int(candidate in served) + int(candidate in raw)
            control(
                promoted in served, "a promoted memory with the same words is served"
            )
        elif scenario == "deleted_then_restated":
            first = harness.remember(TENANT_A, str(payload["own"]))
            harness.sync(TENANT_A)
            control(
                first in harness.served(TENANT_A, str(payload["query"]))[0],
                "the memory is served before it is forgotten",
            )
            forget_memory(harness.fabric[TENANT_A], harness.deletion, first)
            second = harness.remember(TENANT_A, str(payload["own"]))
            harness.sync(TENANT_A)
            served, raw = harness.served(TENANT_A, str(payload["query"]))
            leaks = int(first in served) + int(first in raw)
            control(
                second != first and second in served,
                "the restated memory answers as a new entry",
            )
        else:
            raise ValueError(f"unknown adversarial scenario {scenario!r}")
        if controls:
            notes.append(f"{case.case_id}: control failed: {'; '.join(controls)}")
            leaks += len(controls)
        elif leaks:
            notes.append(f"{case.case_id}: {leaks} served")
        outcome = CaseOutcome(
            dataset_id=dataset.dataset_id,
            case_id=case.case_id,
            protected=True,
            passed=leaks == 0,
        )
        if "cross_tenant" in case.tags:
            foreign_total += leaks
            cross_outcomes.append(outcome)
        else:
            deleted_total += leaks
            deleted_outcomes.append(outcome)
    harness.reset()

    def notes_for(outcomes: Sequence[CaseOutcome]) -> str:
        names = tuple(f"{item.case_id}:" for item in outcomes)
        return "; ".join(note for note in notes if note.startswith(names))

    return (
        MetricResult(
            measurement=Measurement(
                metric=METRIC_CROSS_TENANT,
                value=float(foreign_total),
                unit="count",
                cases=len(cross_outcomes),
                notes=notes_for(cross_outcomes),
            ),
            outcomes=tuple(cross_outcomes),
        ),
        MetricResult(
            measurement=Measurement(
                metric=METRIC_DELETED_AFTER_REFRESH,
                value=float(deleted_total),
                unit="count",
                cases=len(deleted_outcomes),
                notes=notes_for(deleted_outcomes),
            ),
            outcomes=tuple(deleted_outcomes),
        ),
    )


def forge_embedding(harness: MemoryHarness, *, context: str, claimed: str) -> bool:
    """Try to write a derived row for `claimed` under `context`'s session; True when it was accepted."""
    import psycopg

    vector = "[" + ",".join(["0"] * (INDEX_DIMENSIONS - 1) + ["1"]) + "]"
    connection = harness.connect()
    try:
        with connection.cursor() as cursor:
            cursor.execute(
                "SELECT set_config('quansio.tenant_id', %s, true)", (context,)
            )
            cursor.execute(
                "INSERT INTO derived.embeddings (id, tenant_id, workspace_id, source_kind, source_ref,"
                " model_id, dimensions, embedding, snapshot, content_digest, chunk_index)"
                " VALUES (%s, %s, NULL, 'memory', 'mem_forged', %s, %s, %s::vector, 'forged', 'forged', 0)",
                (
                    "emb_01J8Z3K6F1N8VQ2X5W9Y0QA4"
                    + ("AA" if context == TENANT_A else "BB"),
                    claimed,
                    harness.model_id,
                    INDEX_DIMENSIONS,
                    vector,
                ),
            )
        connection.rollback()  # never keep the row: acceptance is what is measured
        return True
    except psycopg.Error:
        connection.rollback()
        return False
    finally:
        connection.close()


def touch_memories(
    harness: MemoryHarness, *, context: str, claimed: str, commit: bool = True
) -> int:
    """Rows of `claimed` an UPDATE under `context`'s session reached (0 when refused)."""
    import psycopg

    connection = harness.connect()
    try:
        with connection.cursor() as cursor:
            cursor.execute(
                "SELECT set_config('quansio.tenant_id', %s, true)", (context,)
            )
            cursor.execute(
                "UPDATE memory_entries SET status = status WHERE tenant_id = %s",
                (claimed,),
            )
            reached = max(cursor.rowcount, 0)
        if commit:
            connection.commit()
        else:
            connection.rollback()
        return reached
    except psycopg.Error:
        connection.rollback()
        return 0
    finally:
        connection.close()


def table_counts(
    harness: MemoryHarness, *, context: str | None, foreign_only: bool = False
) -> dict[str, int]:
    """Rows a `quansio_app` session sees in the memory and derived tables, with no tenant predicate."""
    connection = harness.connect()
    counts: dict[str, int] = {}
    try:
        with connection.cursor() as cursor:
            if context is not None:
                cursor.execute(
                    "SELECT set_config('quansio.tenant_id', %s, true)", (context,)
                )
            for table in ("memory_entries", "derived.embeddings"):
                if foreign_only and context is not None:
                    cursor.execute(
                        f"SELECT count(*) FROM {table} WHERE tenant_id <> %s",
                        (context,),
                    )
                else:
                    cursor.execute(f"SELECT count(*) FROM {table}")
                counts[table] = int(cursor.fetchone()[0])
        connection.rollback()
    finally:
        connection.close()
    return counts


# ============================================================================ baseline and gate


def load_baseline() -> tuple[CaseOutcome, ...]:
    """The frozen protected outcomes the campaign's regressions are counted against."""
    document = json.loads(BASELINE.read_text(encoding="utf-8"))
    return tuple(
        CaseOutcome(
            dataset_id=item["dataset_id"],
            case_id=item["case_id"],
            protected=True,
            passed=item["passed"],
        )
        for item in document["outcomes"]
    )


def baseline_document(outcomes: Sequence[CaseOutcome], *, note: str) -> dict[str, Any]:
    protected = sorted(
        (
            {"dataset_id": o.dataset_id, "case_id": o.case_id, "passed": o.passed}
            for o in outcomes
            if o.protected
        ),
        key=lambda item: (item["dataset_id"], item["case_id"]),
    )
    return {"version": 1, "notes": note, "outcomes": protected}


@dataclass
class Campaign:
    """What one campaign run measured, ready to be gated and written as evidence."""

    measurements: list[Measurement] = field(default_factory=list)
    outcomes: list[CaseOutcome] = field(default_factory=list)
    datasets: list[Dataset] = field(default_factory=list)
    blocked: list[str] = field(default_factory=list)
    versions: dict[str, Any] = field(default_factory=dict)

    def add(self, result: MetricResult, dataset: Dataset | None = None) -> None:
        self.measurements.append(result.measurement)
        self.outcomes.extend(result.outcomes)
        if dataset is not None and dataset.dataset_id not in {
            item.dataset_id for item in self.datasets
        }:
            self.datasets.append(dataset)

    def pin(self, dataset: Dataset) -> None:
        if dataset.dataset_id not in {item.dataset_id for item in self.datasets}:
            self.datasets.append(dataset)

    def measured(self, metric: str) -> Measurement | None:
        return next((item for item in self.measurements if item.metric == metric), None)

    def run(
        self, run_id: str, cost: CostLatency, baseline: Sequence[CaseOutcome]
    ) -> EvaluationRun:
        from intelligence.evaluation.metrics import protected_regressions

        regressions = protected_regressions(baseline, self.outcomes)
        return run_for(
            run_id,
            self.datasets,
            measurements=[*self.measurements, regressions],
            versions=ImplementationVersions(**self.versions),
            cost=cost,
            outcomes=self.outcomes,
        )


@dataclass
class CampaignResult:
    """One campaign: the gated run, its verdict, the campaign's own checks and what was blocked."""

    run: EvaluationRun
    verdict: GateVerdict
    checks: dict[str, dict[str, Any]]
    blocked: list[str]
    details: dict[str, Any]
    ledger: CallLedger

    @property
    def checks_hold(self) -> bool:
        return all(item["held"] for item in self.checks.values())

    def evidence(self) -> dict[str, Any]:
        return {
            "checks": self.checks,
            "blocked_external": self.blocked,
            "details": self.details,
            "model_calls": {
                "calls": self.ledger.calls,
                "input_tokens": self.ledger.input_tokens,
                "output_tokens": self.ledger.output_tokens,
                "cost_minor_units": self.ledger.cost_minor_units,
                "latency_p95_ms": p95(self.ledger.latencies_ms)
                if self.ledger.latencies_ms
                else None,
                "errors": self.ledger.errors[:10],
            },
        }


def trusted(
    result: MetricResult | None, agreement: Measurement, floor: float
) -> MetricResult | None:
    """A measurement from an instrument, only when the instrument met its calibration floor."""
    if result is None or agreement.value < floor:
        return None
    return result


def run_campaign(
    *,
    run_id: str,
    admin_url: str = "",
    environ: Mapping[str, str] | None = None,
    checker: RegistryToolCheck | None = None,
    judge_factory: Callable[[LiveModel, Mapping[str, str], CallLedger, int], Any]
    | None = None,
) -> CampaignResult:
    """Run every leg whose boundary is present and gate the result on DOSSIER §21.3.

    A leg whose boundary is absent is named in `blocked` and its metric stays unmeasured, so the gate
    fails closed on it; nothing is simulated in its place.
    """
    from intelligence.evaluation.metrics import (
        load_corpus,
        measure_injection_corpus,
        measure_route_quality,
        measure_skill_resolution,
    )
    from intelligence.model_gateway.routing import PolicyRouteSelector

    source = os.environ if environ is None else environ
    config = campaign_config()
    live = config["live"]
    sets = campaign_sets()
    campaign = Campaign()
    checks: dict[str, dict[str, Any]] = {}
    blocked: list[str] = []
    details: dict[str, Any] = {}
    ledger = CallLedger(
        max_calls=int(live["max_model_calls"]),
        max_cost_minor_units=int(live["max_cost_minor_units"]),
    )
    checker = checker or RegistryToolCheck()
    slowest_index_ms = 0.0

    def check(name: str, held: bool, observed: object, bar: str) -> None:
        checks[name] = {"held": bool(held), "observed": observed, "bar": bar}

    # -- route: the configured catalog and the deployment's selector ---------------------------------
    gateway = ModelGateway(
        catalog=ModelCatalog.load(), environ={}, selector=PolicyRouteSelector()
    )
    route_sets = (sets["route_matrix"], harness_set("route_quality"))
    route_results = [measure_route_quality(gateway, dataset) for dataset in route_sets]
    route_outcomes = [
        outcome for result in route_results for outcome in result.outcomes
    ]
    held = sum(1 for outcome in route_outcomes if outcome.passed)
    campaign.add(
        MetricResult(
            measurement=Measurement(
                metric="route_determinism",
                value=held / len(route_outcomes),
                unit="ratio",
                cases=len(route_outcomes),
                notes="; ".join(
                    result.measurement.notes
                    for result in route_results
                    if result.measurement.notes
                ),
            ),
            outcomes=tuple(route_outcomes),
        )
    )
    for dataset in route_sets:
        campaign.pin(dataset)

    # -- injection corpus (INT-012) ----------------------------------------------------------------
    malicious, benign = load_corpus(
        ROOT / "tests" / "security" / "injection" / "corpus.json"
    )
    unauthorized, detection = measure_injection_corpus(malicious, benign)
    campaign.add(unauthorized)
    campaign.measurements.append(detection.measurement)

    # -- context budget ------------------------------------------------------------------------------
    context_set = harness_set("context_budget")
    context = evaluate_context_budget(context_set)
    campaign.add(context, context_set)
    check(
        "context_budget_conformance",
        context.measurement.value == 1.0,
        context.measurement.value,
        "= 1.0",
    )

    # -- skills: regression over every shipped pack, resolution behaviour, the WikiSkill campaign -----
    regression, skill_versions = measure_skill_regression()
    campaign.add(regression)
    resolution_set = harness_set("resolution_behaviour")
    resolution = measure_skill_resolution(resolution_set)
    campaign.add(resolution, resolution_set)
    wiki = load_wiki_campaign()
    _wiki_run, wiki_verdict = wiki.run_campaign()
    campaign.outcomes.append(
        CaseOutcome(
            dataset_id="wiki_navigation",
            case_id="campaign",
            protected=True,
            passed=wiki_verdict.passed,
        )
    )
    check(
        "skill_regression",
        regression.measurement.value == 1.0,
        regression.measurement.value,
        "= 1.0",
    )
    check(
        "wiki_campaign",
        wiki_verdict.passed and not wiki_verdict.promotion_blocked,
        wiki_verdict.reason,
        "passes",
    )
    check(
        "skill_resolution",
        resolution.measurement.value == 1.0,
        resolution.measurement.value,
        "= 1.0",
    )

    # -- the Tool Registry check, calibrated -------------------------------------------------------------
    proposal_calibration = sets["proposal_calibration"]
    checker_cal = measure_tool_proposals(proposal_calibration, checker)
    checker_agreement = calibration_agreement(
        METRIC_CHECKER_AGREEMENT, checker_cal, proposal_calibration
    )
    campaign.measurements.append(checker_agreement)
    campaign.pin(proposal_calibration)
    checker_floor = float(config["checker_calibration_min_agreement"])
    check(
        "tool_check_calibration",
        checker_agreement.value >= checker_floor,
        checker_agreement.value,
        f">= {checker_floor}",
    )

    # -- memory isolation and the real derived table -------------------------------------------------------
    corpus = sets["retrieval_corpus"]
    campaign.pin(corpus)
    if admin_url:
        from intelligence.embeddings import SqlEmbeddingStore, index_for

        with scratch_database(admin_url, "campaign") as url:
            harness = MemoryHarness(
                url=url, embedder=HashedEmbedder(), model_id=HASHED_EMBEDDING_ROUTE
            )
            cross, deleted = run_memory_adversarial(
                harness, sets["memory_adversarial"], corpus
            )
            campaign.add(cross, sets["memory_adversarial"])
            campaign.add(deleted)
            index = index_for(
                SqlEmbeddingStore(app_role_connect(url)),
                HashedEmbedder(),
                model_id=HASHED_EMBEDDING_ROUTE,
                tenant_id=TENANT_A,
            )
            hashed_recall, _ = measure_corpus_recall(
                index, corpus, k=int(config["retrieval"]["k"])
            )
            latencies: list[float] = []
            queries = [query for query, _expected in corpus_queries(corpus)]
            for sample in range(int(config["retrieval"]["latency_samples"])):
                started = time.perf_counter()
                index.query(
                    queries[sample % len(queries)], limit=int(config["retrieval"]["k"])
                )
                latencies.append((time.perf_counter() - started) * 1000)
            slo = index_query_slo_ms()
            within = sum(1 for value in latencies if value <= slo) / len(latencies)
            slowest_index_ms = max(latencies)
            campaign.measurements.append(
                Measurement(
                    metric="index_query_within_slo",
                    value=within,
                    unit="ratio",
                    cases=len(latencies),
                    notes=f"p95 {p95(latencies):.1f} ms against DOSSIER 21.2 index query <= {slo:.0f} ms",
                )
            )
            check(
                "index_query_p95",
                within >= 0.95,
                round(p95(latencies), 1),
                f"p95 <= {slo:.0f} ms (21.2)",
            )
            details["hashed_embedder_recall_at_10"] = {
                "value": hashed_recall.measurement.value,
                "notes": "deterministic bag-of-words embedder over the real pgvector table; a harness "
                "self-check, not the recall metric (that needs the configured embedding route)",
            }
    else:
        blocked.append(
            "QUANSIO_TEST_POSTGRES_URL (memory isolation adversarial set, derived-table latency)"
        )

    embedder = live_embedder(source)
    if isinstance(embedder, str):
        blocked.append(
            f"{embedder} (retrieval recall@10 with the configured embedding route)"
        )
    elif not admin_url:
        blocked.append(
            "QUANSIO_TEST_POSTGRES_URL (retrieval recall@10 on the real derived table)"
        )
    else:
        from intelligence.embeddings import SqlEmbeddingStore, index_for

        live_embed, embedding_route = embedder
        with scratch_database(admin_url, "recall") as url:
            index = index_for(
                SqlEmbeddingStore(app_role_connect(url)),
                live_embed,
                model_id=embedding_route,
                tenant_id=TENANT_A,
            )
            recall, _latencies = measure_corpus_recall(
                index, corpus, k=int(config["retrieval"]["k"])
            )
        campaign.add(recall)
        campaign.versions["embedding_route"] = embedding_route

    # -- live model legs -------------------------------------------------------------------------------------
    answering = live_model(ProviderKind.OPENAI_COMPATIBLE, source)
    judging = live_model(ProviderKind.ANTHROPIC, source)
    timeout_ms = int(live["timeout_ms"])
    max_output = int(live["max_output_tokens"])
    if isinstance(answering, str):
        blocked.append(
            f"{answering} (live answers, tool proposals, context accepted by a provider)"
        )
    else:
        live_gateway = answering.gateway()
        campaign.versions["model_route"] = answering.model_id
        campaign.versions["provider"] = f"{answering.kind.value}:{answering.wire_model}"
        # Tool proposals, judged by the registry — only when the registry check is calibrated.
        tasks = sets["proposal_tasks"]
        replies = {
            case.case_id: propose_tool_calls(
                answering,
                live_gateway,
                case,
                ledger,
                timeout_ms=timeout_ms,
                max_output_tokens=max_output,
            )
            for case in tasks.cases
        }
        proposals = measure_live_proposals(replies, checker, tasks)
        campaign.pin(tasks)
        details["tool_proposals"] = {
            case_id: [{"tool": name, "args": args} for name, args in reply.tool_calls]
            for case_id, reply in replies.items()
        }
        accepted = trusted(proposals, checker_agreement, checker_floor)
        if accepted is not None:
            campaign.add(accepted)
        # Research answers through the context plane.
        research = sets["research_questions"]
        evidence = corpus_documents(corpus)
        claims: dict[str, tuple[GeneratedClaim, ...]] = {}
        ratios: list[float] = []
        for case in research.cases:
            generated, estimated, provider_tokens = answer_with_citations(
                answering,
                live_gateway,
                case,
                evidence,
                ledger,
                timeout_ms=timeout_ms,
                max_output_tokens=max_output,
            )
            claims[case.case_id] = generated
            if provider_tokens:
                ratios.append(provider_tokens / estimated)
        campaign.pin(research)
        details["generated_claims"] = {
            case_id: [
                {"text": claim.text, "citations": list(claim.citations)}
                for claim in items
            ]
            for case_id, items in claims.items()
        }
        worst = max(ratios) if ratios else math.inf
        details["context_provider_token_ratios"] = [round(value, 3) for value in ratios]
        check(
            "context_estimate_covers_provider_count",
            bool(ratios) and worst <= float(config["max_provider_token_ratio"]),
            round(worst, 3),
            f"provider input tokens / plane estimate <= {config['max_provider_token_ratio']}",
        )
        if isinstance(judging, str):
            blocked.append(f"{judging} (the grounding judge)")
        else:
            calibration = sets["grounding_calibration"]
            if judge_factory is None:
                judge: Any = ModelGroundingJudge(
                    model=judging,
                    evidence=evidence,
                    ledger=ledger,
                    timeout_ms=timeout_ms,
                )
            else:
                judge = judge_factory(judging, evidence, ledger, timeout_ms)
            judge_cal = measure_grounding(calibration, judge)
            agreement = calibration_agreement(
                METRIC_JUDGE_AGREEMENT, judge_cal, calibration
            )
            campaign.measurements.append(agreement)
            campaign.pin(calibration)
            floor = float(config["judge_calibration_min_agreement"])
            check(
                "grounding_judge_calibration",
                agreement.value >= floor,
                agreement.value,
                f">= {floor}",
            )
            if agreement.value >= floor:
                campaign.add(measure_unsupported_claims(claims, judge, research))
            details["judge"] = {
                "route": judging.model_id,
                "wire_model": judging.wire_model,
                "malformed_verdicts": getattr(judge, "malformed", 0),
                "independence": "separate call, prompt and wire from the answering model; same weights when "
                "both boundaries serve the same model, which is why the judge is calibrated first",
            }

    check(
        "campaign_budget",
        ledger.calls <= ledger.max_calls
        and ledger.cost_minor_units <= ledger.max_cost_minor_units,
        {"calls": ledger.calls, "cost_minor_units": ledger.cost_minor_units},
        f"<= {ledger.max_calls} calls, <= {ledger.max_cost_minor_units} minor units",
    )
    baseline = load_baseline()
    present = {
        (item.dataset_id, item.case_id) for item in campaign.outcomes if item.protected
    }
    missing = sorted(
        f"{d}/{c}" for d, c in {(b.dataset_id, b.case_id) for b in baseline} - present
    )
    covered_by_run = [
        name
        for name in missing
        if not name.startswith("memory_adversarial/") or admin_url
    ]
    check(
        "baseline_cases_present",
        not covered_by_run,
        covered_by_run[:6],
        "every frozen protected case is run",
    )

    campaign.versions.setdefault(
        "embedding_route", HASHED_EMBEDDING_ROUTE if admin_url else ""
    )
    campaign.versions["index_snapshot"] = SNAPSHOT
    campaign.versions["skill_versions"] = tuple(sorted(skill_versions.items()))
    campaign.versions["harness_version"] = "qa004-v1"
    run = campaign.run(run_id, ledger.cost(slowest_index_ms), baseline)
    return CampaignResult(
        run=run,
        verdict=gate(run),
        checks=checks,
        blocked=blocked,
        details=details,
        ledger=ledger,
    )


def load_wiki_campaign() -> Any:
    import importlib.util

    path = PACKS / "wiki" / "navigate.py"
    spec = importlib.util.spec_from_file_location("qa004_wiki_navigate", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def gate(run: EvaluationRun) -> GateVerdict:
    return evaluate_gate(run, ThresholdSet.load(THRESHOLDS))


def write_evidence(
    directory: Path, run: EvaluationRun, verdict: GateVerdict, extra: Mapping[str, Any]
) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "campaign-run.json").write_text(
        json.dumps(run.as_mapping(), indent=2, sort_keys=True) + "\n"
    )
    (directory / "campaign-verdict.json").write_text(
        json.dumps({"verdict": verdict.as_mapping(), **extra}, indent=2, sort_keys=True)
        + "\n"
    )


__all__ = [
    "BASELINE",
    "CallLedger",
    "Campaign",
    "HashedEmbedder",
    "LiveModel",
    "MemoryHarness",
    "ModelGroundingJudge",
    "RegistryToolCheck",
    "answer_with_citations",
    "calibration_agreement",
    "campaign_config",
    "campaign_sets",
    "evaluate_context_budget",
    "gate",
    "harness_set",
    "index_query_slo_ms",
    "live_embedder",
    "live_model",
    "measure_corpus_recall",
    "measure_grounding",
    "measure_live_proposals",
    "measure_skill_regression",
    "measure_tool_proposals",
    "measure_unsupported_claims",
    "p95",
    "propose_tool_calls",
    "run_memory_adversarial",
    "scratch_database",
    "write_evidence",
]
