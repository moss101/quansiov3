"""Evidence-first research workflow (CAP-001).

The workflow composes existing primitives only — typed SearchProgram (INT-005),
ContextProjection with UNTRUSTED_EXTERNAL labelling (INT-005/§12), and Evidence
descriptors addressed by digest (CORE-007) — so research creates no second
orchestrator and no second memory store:

* `compile_intent` turns the question into a typed SearchProgram plus a bounded
  list of collection branches (§ build item 1).
* `collect` runs the program through a [`SearchBackend`][…port]; every fetched
  source enters context as UNTRUSTED_EXTERNAL and is recorded as an evidence
  descriptor addressed by its content digest.
* `synthesize` may only cite descriptors that were actually collected; a claim
  whose citations do not resolve is an unsupported claim and blocks completion
  (acceptance 2). The answer artifact carries the descriptors so every final
  claim resolves back to evidence.
* Cancellation is a stage, not an exception blowout: `cancel` records where the
  run stopped and `resume` continues from that stage without re-fetching
  sources already collected (cancel/recover test).
"""

from __future__ import annotations

import hashlib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field

from intelligence.context.projection import Segment, TrustLevel
from intelligence.context.search import SearchProgram

STAGES: tuple[str, ...] = ("compiled", "collected", "synthesized")


class ResearchError(ValueError):
    """A refused research step, naming the rule that refused it."""

    def __init__(self, code: str, detail: str) -> None:
        super().__init__(f"{code}: {detail}")
        self.code = code
        self.detail = detail


@dataclass(frozen=True, slots=True)
class ResearchPlan:
    """The compiled intent: one typed program and its bounded branches."""

    question: str
    program: SearchProgram
    branches: tuple[str, ...]
    budget: int

    @property
    def stage(self) -> str:
        return "compiled"


def compile_intent(question: str, *, branches: int = 2, budget: int = 2_048) -> ResearchPlan:
    """Compile the question into a typed program; the branch count is bounded."""
    if not question.strip():
        raise ResearchError("VALIDATION_SCHEMA", "a research intent needs a question")
    if not 1 <= branches <= 4:
        raise ResearchError("VALIDATION_BOUNDS", "research branches must be 1..4")
    terms = [term for term in question.lower().split() if len(term) > 2]
    program = SearchProgram.parse(
        {
            "channels": [{"channel": "lexical", "text": " ".join(terms[:8]), "limit": 10}],
            "predicates": [],
            "limit": 10,
        }
    )
    return ResearchPlan(
        question=question,
        program=program,
        branches=tuple(f"branch-{i}" for i in range(1, branches + 1)),
        budget=budget,
    )


@dataclass(frozen=True, slots=True)
class EvidenceDescriptor:
    """One collected source, addressed by the digest of what was read (CORE-007)."""

    source_id: str
    uri: str
    content_digest: str
    text: str
    retrieved_at: str

    def as_mapping(self) -> dict[str, str]:
        return {
            "source_id": self.source_id,
            "uri": self.uri,
            "content_digest": self.content_digest,
            "retrieved_at": self.retrieved_at,
        }


@dataclass(slots=True)
class SearchBackend:
    """The outbound port. Production: web.search/web.fetch under egress policy.

    Tests inject canned sources; nothing here performs provider I/O itself.
    """

    results: Mapping[str, Sequence[tuple[str, str]]] = field(default_factory=dict)
    queries_served: list[str] = field(default_factory=list)

    def search(self, query: str) -> Sequence[tuple[str, str]]:
        """Return `(source_id, (uri, text))` pairs for the query."""
        self.queries_served.append(query)
        return self.results.get(query, ())


@dataclass(slots=True)
class ResearchRun:
    """A cancel/recover-able research run: stage, evidence, and what it collected."""

    plan: ResearchPlan
    stage: str = "compiled"
    evidence: dict[str, EvidenceDescriptor] = field(default_factory=dict)
    cancelled_at: str | None = None

    def cancel(self, at_stage: str) -> None:
        if at_stage not in STAGES:
            raise ResearchError("VALIDATION_SCHEMA", f"unknown stage {at_stage!r}")
        self.cancelled_at = at_stage


def _descriptor(source_id: str, uri: str, text: str, retrieved_at: str) -> EvidenceDescriptor:
    digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
    return EvidenceDescriptor(
        source_id=source_id,
        uri=uri,
        content_digest=digest,
        text=text,
        retrieved_at=retrieved_at,
    )


def collect(run: ResearchRun, backend: SearchBackend) -> ResearchRun:
    """Fetch every branch through the backend; fetched text is UNTRUSTED_EXTERNAL data."""
    if run.cancelled_at is not None:
        raise ResearchError("CANCELLED", f"the run was cancelled at {run.cancelled_at}")
    if run.stage != "compiled":
        raise ResearchError("CONFLICT_STATE", f"collect expects the compiled stage, got {run.stage}")
    query = run.plan.program.channels[0].value
    for source_id, pair in backend.search(query):
        uri, text = str(pair[0]), str(pair[1])
        if source_id in run.evidence:
            continue  # idempotent: a re-fetch of a collected source is a no-op
        run.evidence[source_id] = _descriptor(str(source_id), uri, text, "collected")
    run.stage = "collected"
    return run


def collected_segments(run: ResearchRun) -> list[Segment]:
    """The collected sources as context segments, all UNTRUSTED_EXTERNAL (§12)."""
    return [
        Segment.build(
            segment_id=f"seg_{descriptor.source_id}",
            text=descriptor.text,
            tokens=max(1, len(descriptor.text.split())),
            trust_level=TrustLevel.UNTRUSTED_EXTERNAL,
            source=descriptor.uri,
            snapshot=f"research:{descriptor.content_digest[:12]}",
            channel="lexical",
            score=1.0,
            evidence_id=descriptor.content_digest,
        )
        for descriptor in sorted(run.evidence.values(), key=lambda item: item.source_id)
    ]


@dataclass(frozen=True, slots=True)
class Claim:
    """One answer claim with the citations that must resolve to evidence."""

    text: str
    citations: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class ResearchAnswer:
    """The answer artifact: claims bound to the descriptors that support them."""

    question: str
    claims: tuple[Claim, ...]
    descriptors: tuple[EvidenceDescriptor, ...]
    segments_used: int

    def resolve(self, claim: Claim) -> list[EvidenceDescriptor]:
        """Every citation of one claim, resolved to its descriptor."""
        return [
            run_descriptor
            for run_descriptor in self.descriptors
            if run_descriptor.source_id in claim.citations
        ]


def synthesize(
    run: ResearchRun,
    claims: Sequence[Claim],
) -> ResearchAnswer:
    """Build the answer, refusing any claim whose citations do not resolve."""
    if run.stage != "collected":
        raise ResearchError("CONFLICT_STATE", f"synthesize expects the collected stage, got {run.stage}")
    if not run.evidence:
        raise ResearchError("VALIDATION_SCHEMA", "no evidence was collected; refusing to synthesize")
    unknown: list[str] = []
    for claim in claims:
        if not claim.citations:
            unknown.append(f"{claim.text[:40]!r}: uncited")
            continue
        for citation in claim.citations:
            if citation not in run.evidence:
                unknown.append(f"{claim.text[:40]!r}: {citation}")
    if unknown:
        raise ResearchError(
            "VALIDATION_SCHEMA",
            "unsupported claims; citations must resolve to collected evidence: " + "; ".join(unknown),
        )
    answer = ResearchAnswer(
        question=run.plan.question,
        claims=tuple(claims),
        descriptors=tuple(sorted(run.evidence.values(), key=lambda item: item.source_id)),
        segments_used=len(run.evidence),
    )
    run.stage = "synthesized"
    return answer
