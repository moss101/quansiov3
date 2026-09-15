"""WikiSkill navigation procedure (CAP-006).

This is not a search subsystem. It composes the existing typed SearchProgram
(INT-005), Knowledge Fabric entries (INT-006) and ContextProjection packing
(INT-005). Hits that left retrieval (`deleted` / not `active`) never appear.
Every hit keeps its provenance. Summarization is budgeted packing, not a new store.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml

from intelligence.context.projection import ContextBundle, ContextPolicy, Segment, TrustLevel, build_bundle
from intelligence.context.search import Predicate, SearchProgram
from intelligence.knowledge.models import KnowledgeEntry, KnowledgeScope, KnowledgeStatus, Provenance
from intelligence.skills.models import SkillManifest

PACK_DIR = Path(__file__).resolve().parent
EVALS_DIR = PACK_DIR / "evals"
CORPUS_PATH = EVALS_DIR / "corpus.json"
THRESHOLDS_PATH = EVALS_DIR / "thresholds.yaml"
SKILL_PATH = PACK_DIR / "skill.yaml"

TENANT = "tn_wiki_eval"


@dataclass(frozen=True, slots=True)
class WikiPage:
    """One pinned wiki page: fabric entry plus the body and links the eval corpus holds."""

    entry: KnowledgeEntry
    title: str
    body: str
    links: tuple[str, ...]

    @property
    def id(self) -> str:
        return self.entry.id

    @property
    def retrievable(self) -> bool:
        return self.entry.retrievable


@dataclass(frozen=True, slots=True)
class WikiHit:
    """A retrieved page with the provenance the fabric stored, never stripped."""

    page_id: str
    title: str
    body: str
    links: tuple[str, ...]
    provenance: tuple[Provenance, ...]
    score: float

    def provenance_intact(self) -> bool:
        return bool(self.provenance) and all(
            item.source_kind.strip() and item.ref.strip() and len(item.digest) == 64
            for item in self.provenance
        )


@dataclass(frozen=True, slots=True)
class NavigationResult:
    """What WikiSkill selected, and the budgeted projection it packed."""

    program: SearchProgram
    hits: tuple[WikiHit, ...]
    bundle: ContextBundle
    expected_ids: tuple[str, ...] = ()

    @property
    def hit_ids(self) -> tuple[str, ...]:
        return tuple(hit.page_id for hit in self.hits)

    def recall(self) -> float:
        if not self.expected_ids:
            return 1.0
        found = set(self.hit_ids)
        return sum(1 for item in self.expected_ids if item in found) / len(self.expected_ids)

    def provenance_rate(self) -> float:
        if not self.hits:
            return 1.0
        return sum(1 for hit in self.hits if hit.provenance_intact()) / len(self.hits)

    def link_rate(self, pages: Mapping[str, WikiPage]) -> float:
        if not self.hits:
            return 1.0
        intact = 0
        for hit in self.hits:
            declared = pages[hit.page_id].links
            intact += int(hit.links == declared)
        return intact / len(self.hits)

    def budget_ok(self) -> bool:
        return self.bundle.ledger.used <= self.bundle.ledger.budget


def load_skill_manifest(path: Path | None = None) -> SkillManifest:
    raw = yaml.safe_load((path or SKILL_PATH).read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ValueError("WikiSkill skill.yaml must be an object")
    return SkillManifest.parse(raw)


def load_corpus(path: Path | None = None) -> dict[str, WikiPage]:
    payload = json.loads((path or CORPUS_PATH).read_text(encoding="utf-8"))
    pages: dict[str, WikiPage] = {}
    for raw in payload["pages"]:
        status = KnowledgeStatus(str(raw["status"]))
        provenance = Provenance(
            source_kind=str(raw["provenance"]["source_kind"]),
            ref=str(raw["provenance"]["ref"]),
            digest=str(raw["provenance"]["digest"]),
        )
        entry = KnowledgeEntry(
            id=str(raw["id"]),
            tenant_id=str(payload.get("tenant_id", TENANT)),
            scope=KnowledgeScope.PACK,
            kind=str(raw["kind"]),
            provenance=(provenance,),
            content_ref=f"wiki://{raw['id']}",
            status=status,
            confidence=1.0,
        )
        pages[entry.id] = WikiPage(
            entry=entry,
            title=str(raw["title"]),
            body=str(raw["body"]),
            links=tuple(str(item) for item in raw.get("links", [])),
        )
    return pages


def program_for(query: str, *, limit: int = 10) -> SearchProgram:
    """A typed program for a wiki navigation query. Never a predicate string."""
    return SearchProgram.parse(
        {
            "channels": [{"channel": "lexical", "text": query, "limit": limit}],
            "predicates": [{"field": "kind", "operator": "eq", "value": "wiki_page"}],
            "limit": limit,
        }
    )


def _tokens(text: str) -> int:
    return max(1, len(text.split()))


def _matches_term(page: WikiPage, term: str) -> bool:
    haystack = f"{page.title}\n{page.body}".lower()
    return term.lower() in haystack


def _predicate_holds(page: WikiPage, predicate: Predicate) -> bool:
    if predicate.field == "kind":
        actual: Any = page.entry.kind
    elif predicate.field == "title":
        actual = page.title
    elif predicate.field == "status":
        actual = page.entry.status.value
    else:
        return True
    if predicate.operator == "eq":
        return actual == predicate.value
    if predicate.operator == "contains":
        return isinstance(actual, str) and str(predicate.value).lower() in actual.lower()
    if predicate.operator == "in":
        return actual in predicate.value
    return True


def search_pages(pages: Mapping[str, WikiPage], program: SearchProgram) -> tuple[WikiHit, ...]:
    """Retrieve active pages that satisfy the typed program. Deleted pages never hit."""
    hits: list[WikiHit] = []
    for page in pages.values():
        if not page.retrievable:
            continue
        if any(not _predicate_holds(page, predicate) for predicate in program.predicates):
            continue
        score = 0.0
        for term in program.channels:
            if term.channel.value not in {"lexical", "semantic", "exact"}:
                continue
            if not _matches_term(page, term.value):
                continue
            if term.channel.value == "exact" and page.title.lower() == term.value.lower():
                score += 2.0
            else:
                score += 1.0
        if score <= 0.0:
            continue
        hits.append(
            WikiHit(
                page_id=page.id,
                title=page.title,
                body=page.body,
                links=page.links,
                provenance=page.entry.provenance,
                score=score,
            )
        )
    hits.sort(key=lambda item: (-item.score, item.page_id))
    return tuple(hits[: program.limit])


def pack_hits(hits: Sequence[WikiHit], *, budget: int, snapshot: str, program: SearchProgram) -> ContextBundle:
    """Summarize selected sources into a budgeted projection; record what was dropped."""
    segments = [
        Segment.build(
            segment_id=f"seg_{hit.page_id}",
            text=f"{hit.title}\n{hit.body}",
            tokens=_tokens(hit.body),
            trust_level=TrustLevel.VERIFIED_KNOWLEDGE,
            source=hit.provenance[0].ref if hit.provenance else hit.page_id,
            snapshot=snapshot,
            channel="lexical",
            score=hit.score,
            evidence_id=hit.provenance[0].digest if hit.provenance else None,
        )
        for hit in hits
    ]
    return build_bundle(
        program_key=program.canonical_json(),
        snapshot_id=snapshot,
        policy=ContextPolicy(token_budget=budget, min_trust=TrustLevel.VERIFIED_KNOWLEDGE),
        segments=segments,
        current_snapshot=snapshot,
    )


def navigate(
    query: str,
    *,
    pages: Mapping[str, WikiPage] | None = None,
    budget: int = 128,
    expected_ids: Sequence[str] = (),
    snapshot: str = "wiki_eval_v1",
) -> NavigationResult:
    loaded = pages if pages is not None else load_corpus()
    program = program_for(query)
    hits = search_pages(loaded, program)
    bundle = pack_hits(hits, budget=budget, snapshot=snapshot, program=program)
    return NavigationResult(
        program=program,
        hits=hits,
        bundle=bundle,
        expected_ids=tuple(expected_ids),
    )


def run_campaign(
    *,
    dataset_path: Path | None = None,
    pages: Mapping[str, WikiPage] | None = None,
) -> tuple[Any, Any]:
    """Run the pinned WikiSkill eval campaign through INT-010's gate."""
    from intelligence.evaluation.datasets import dataset_from_mapping
    from intelligence.evaluation.gate import ThresholdSet, evaluate_gate
    from intelligence.evaluation.runs import (
        CaseOutcome,
        ImplementationVersions,
        Measurement,
        run_for,
    )

    loaded = pages if pages is not None else load_corpus()
    dataset = dataset_from_mapping(
        json.loads((dataset_path or (EVALS_DIR / "navigation.v1.json")).read_text(encoding="utf-8")),
        source="packs/skills/wiki/evals/navigation.v1.json",
    )
    navigation_pass = 0
    provenance_pass = 0
    link_pass = 0
    budget_pass = 0
    outcomes: list[CaseOutcome] = []
    for case in dataset.cases:
        payload = case.payload
        query = str(payload["query"])
        expected = tuple(str(item) for item in payload.get("expected_ids", []))  # type: ignore[arg-type]
        budget = int(payload.get("budget", 64))  # type: ignore[arg-type]
        forbidden = tuple(str(item) for item in payload.get("forbidden_ids", []))  # type: ignore[arg-type]
        result = navigate(query, pages=loaded, budget=budget, expected_ids=expected)
        recalled = result.recall() >= 1.0
        if expected == () and forbidden:
            recalled = all(item not in result.hit_ids for item in forbidden)
        provenance_ok = result.provenance_rate() >= 1.0
        links_ok = result.link_rate(loaded) >= 1.0
        budget_ok = result.budget_ok()
        navigation_pass += int(recalled)
        provenance_pass += int(provenance_ok)
        link_pass += int(links_ok)
        budget_pass += int(budget_ok)
        outcomes.append(
            CaseOutcome(
                dataset_id=dataset.dataset_id,
                case_id=case.case_id,
                protected=case.protected,
                passed=recalled and provenance_ok and budget_ok,
            )
        )
    n = len(dataset.cases)
    measurements = (
        Measurement(metric="navigation_recall", value=navigation_pass / n, unit="ratio", cases=n),
        Measurement(metric="provenance_preserved", value=provenance_pass / n, unit="ratio", cases=n),
        Measurement(metric="link_preservation", value=link_pass / n, unit="ratio", cases=n),
        Measurement(metric="budget_respected", value=budget_pass / n, unit="ratio", cases=n),
    )
    run = run_for(
        "eval_wiki_navigation",
        [dataset],
        measurements=measurements,
        versions=ImplementationVersions(skill_versions=(("skl_wiki", "sklv_wiki_1"),), index_snapshot="wiki_eval_v1"),
        outcomes=outcomes,
    )
    thresholds = ThresholdSet.load(THRESHOLDS_PATH)
    return run, evaluate_gate(run, thresholds)
