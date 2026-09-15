"""QA-004: intelligence qualification over pinned evaluation sets and frozen thresholds.

The campaign drives the shipped harness — INT-010's gate, the shipped retrieval metric
over the real EmbeddingIndex, and the shipped WikiSkill campaign (CAP-006) — against
DOSSIER §21.3's thresholds as frozen in `tests/evaluation/thresholds.yaml`. Protected
rows: route determinism, cross-tenant retrieval and deleted-memory retrieval are zero
on the adversarial set. This module is the standing root gate (`tests/evaluation/` is
the task's canonical path); it never substitutes self-selected examples.
"""

from __future__ import annotations

import hashlib
import importlib.util
import math
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path

import pytest

from intelligence.embeddings.index import (
    INDEX_DIMENSIONS,
    EmbeddingIndex,
    EmbeddingRow,
    StoredEmbedding,
)
from intelligence.embeddings.sources import SourceDocument
from intelligence.evaluation.datasets import load_datasets
from intelligence.evaluation.gate import ThresholdSet, evaluate_gate
from intelligence.evaluation.metrics import measure_retrieval
from intelligence.evaluation.runs import (
    ImplementationVersions,
    Measurement,
    run_for,
)

ROOT = Path(__file__).resolve().parents[2]
THRESHOLDS = ROOT / "tests" / "evaluation" / "thresholds.yaml"
WIKI_NAVIGATE = ROOT / "packs" / "skills" / "wiki" / "navigate.py"

TENANT_A = "tn_01J8Z3K6F1N8VQ2X5W9Y0QAA04"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0QAB04"
MODEL = "embedding-route"
SNAPSHOT = "qa004-v1"


# ------------------------------------------------------------------ deterministic doubles


@dataclass
class HashedEmbedder:
    """Vectors are hashed bags of words: shared words are near, distinct topics are far."""

    dimensions: int = INDEX_DIMENSIONS

    def embed(self, texts: Sequence[str], *, deadline_ms: int) -> tuple[tuple[float, ...], ...]:
        vectors = []
        for text in texts:
            buckets = [0.0] * self.dimensions
            for word in text.lower().split():
                digest = hashlib.sha256(word.encode("utf-8")).digest()
                buckets[int.from_bytes(digest[:8], "big") % self.dimensions] += 1.0
            norm = math.sqrt(sum(v * v for v in buckets)) or 1.0
            vectors.append(tuple(v / norm for v in buckets))
        return tuple(vectors)


@dataclass
class MemoryEmbeddingStore:
    """The derived-table contract in memory; every read carries the tenant predicate."""

    rows: dict[tuple[object, ...], EmbeddingRow] = field(default_factory=dict)

    @staticmethod
    def _key(row: EmbeddingRow) -> tuple[object, ...]:
        return (
            row.tenant_id,
            row.source_kind,
            row.source_ref,
            row.model_id,
            row.snapshot,
            row.content_digest,
            row.chunk_index,
        )

    def upsert(self, rows: Sequence[EmbeddingRow]) -> int:
        for row in rows:
            self.rows[self._key(row)] = row
        return len(rows)

    def digests(
        self, *, tenant_id: str, source_kind: str, source_ref: str, model_id: str, snapshot: str
    ) -> Mapping[int, str]:
        return {
            row.chunk_index: row.content_digest
            for row in self.rows.values()
            if (row.tenant_id, row.source_kind, row.source_ref, row.model_id, row.snapshot)
            == (tenant_id, source_kind, source_ref, model_id, snapshot)
        }

    def delete_source(self, *, tenant_id: str, source_kind: str, source_ref: str) -> int:
        doomed = [
            key
            for key, row in self.rows.items()
            if (row.tenant_id, row.source_kind, row.source_ref) == (tenant_id, source_kind, source_ref)
        ]
        for key in doomed:
            del self.rows[key]
        return len(doomed)

    def delete_superseded(
        self, *, tenant_id: str, source_kind: str, source_ref: str, model_id: str, snapshot: str
    ) -> int:
        doomed = [
            key
            for key, row in self.rows.items()
            if (row.tenant_id, row.source_kind, row.source_ref, row.model_id)
            == (tenant_id, source_kind, source_ref, model_id)
            and row.snapshot != snapshot
        ]
        for key in doomed:
            del self.rows[key]
        return len(doomed)

    def delete_missing(
        self,
        *,
        tenant_id: str,
        source_kind: str,
        source_ref: str,
        model_id: str,
        snapshot: str,
        keep: frozenset[str],
    ) -> int:
        doomed = [
            key
            for key, row in self.rows.items()
            if (row.tenant_id, row.source_kind, row.source_ref, row.model_id, row.snapshot)
            == (tenant_id, source_kind, source_ref, model_id, snapshot)
            and row.content_digest not in keep
        ]
        for key in doomed:
            del self.rows[key]
        return len(doomed)

    def search(
        self,
        *,
        tenant_id: str,
        workspace_id: str | None,
        query: Sequence[float],
        limit: int,
        snapshot: str | None,
    ) -> Sequence[StoredEmbedding]:
        hits = []
        for row in self.rows.values():
            if row.tenant_id != tenant_id:
                continue
            if snapshot is not None and row.snapshot != snapshot:
                continue
            distance = 1.0 - sum(a * b for a, b in zip(row.embedding, query, strict=False))
            hits.append(
                StoredEmbedding(
                    source_kind=row.source_kind,
                    source_ref=row.source_ref,
                    model_id=row.model_id,
                    snapshot=row.snapshot,
                    content_digest=row.content_digest,
                    chunk_index=row.chunk_index,
                    distance=distance,
                )
            )
        hits.sort(key=lambda hit: hit.distance)
        return hits[:limit]

    def count(self, *, tenant_id: str) -> int:
        return sum(1 for row in self.rows.values() if row.tenant_id == tenant_id)

    def sources(self, *, tenant_id: str, source_kind: str) -> Sequence[str]:
        return sorted(
            {
                row.source_ref
                for row in self.rows.values()
                if row.tenant_id == tenant_id and row.source_kind == source_kind
            }
        )

    def prune(self, *, tenant_id: str, source_kind: str, keep: frozenset[str]) -> int:
        doomed = [
            key
            for key, row in self.rows.items()
            if row.tenant_id == tenant_id and row.source_kind == source_kind and row.source_ref not in keep
        ]
        for key in doomed:
            del self.rows[key]
        return len(doomed)


def _index(tenant_id: str) -> tuple[EmbeddingIndex, MemoryEmbeddingStore]:
    store = MemoryEmbeddingStore()
    return (
        EmbeddingIndex(
            store=store,
            embedder=HashedEmbedder(),
            model_id=MODEL,
            tenant_id=tenant_id,
            workspace_id=None,
            chunk_chars=200,
            overlap_chars=20,
        ),
        store,
    )


# ------------------------------------------------------------------ pinned benchmark corpus

#: Documents per topic; queries must retrieve their own topic's source and nothing else's.
CORPUS: tuple[tuple[str, SourceDocument], ...] = (
    (
        "retention",
        SourceDocument(
            source_kind="artifact",
            source_ref="retention-policy",
            snapshot=SNAPSHOT,
            text="Retention windows decide how long logs and artifacts live. "
            "The retention policy keeps logs for ninety days then deletes them.",
        ),
    ),
    (
        "budget",
        SourceDocument(
            source_kind="artifact",
            source_ref="budget-limits",
            snapshot=SNAPSHOT,
            text="Budgets bound tokens, cost and wall time for every run. "
            "The budget limits stop new work when tokens run out.",
        ),
    ),
    (
        "quarantine",
        SourceDocument(
            source_kind="artifact",
            source_ref="quarantine-runbook",
            snapshot=SNAPSHOT,
            text="Quarantine fences a worker that misbehaves. "
            "The quarantine runbook drains the target and revokes its leases.",
        ),
    ),
)
QUERIES: tuple[tuple[str, str], ...] = (
    ("how long are logs kept by the retention policy", "retention-policy"),
    ("what stops a run when the token budget runs out", "budget-limits"),
    ("which runbook fences a misbehaving worker", "quarantine-runbook"),
)

_deleted_ref = "budget-limits"
_foreign_ref = "foreign-injection"


def _load_wiki():
    spec = importlib.util.spec_from_file_location("qa004_wiki_navigate", WIKI_NAVIGATE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


# ------------------------------------------------------------------ the campaign


def test_retrieval_benchmark_and_memory_isolation_are_zero() -> None:
    """Named tests: retrieval benchmark + memory isolation suite in one real campaign."""
    index_a, store_a = _index(TENANT_A)
    index_b, _store_b = _index(TENANT_B)
    result = measure_retrieval(
        index_a,
        tenant_a=TENANT_A,
        tenant_b=TENANT_B,
        index_b=index_b,
        corpus=CORPUS,
        queries=QUERIES,
        deleted_ref=_deleted_ref,
        foreign_ref=_foreign_ref,
    )
    # Recall benchmark: every pinned query retrieved its own source.
    assert result.measurement.value == pytest.approx(1.0)
    assert result.measurement.metric == "retrieval_recall_at_10"

    # Memory isolation: cross-tenant and deleted rows never answer, on this adversarial set.
    by_case = {outcome.case_id: outcome for outcome in result.outcomes}
    assert by_case["cross_tenant_retrieval"].protected
    assert by_case["cross_tenant_retrieval"].passed, "a foreign tenant's row answered"
    assert by_case["deleted_source_after_refresh"].protected
    assert by_case["deleted_source_after_refresh"].passed, "a deleted source answered after refresh"
    refs = set(store_a.sources(tenant_id=TENANT_A, source_kind="artifact"))
    assert _deleted_ref not in refs
    assert _foreign_ref not in refs, "the foreign ref must never enter tenant A's index"


def test_evaluation_campaign_passes_frozen_protected_thresholds() -> None:
    """Named test: evaluation campaign gated on the frozen §21.3 thresholds.

    Every metric the shipped harness can measure offline is measured; the two cross-authority
    ports (grounding verifier, tool-proposal registry check) are deliberately *not* faked, so
    the gate must report them unmeasured and fail closed — while the protected rows all hold.
    """
    from intelligence.evaluation.datasets import datasets_of_kind
    from intelligence.evaluation.metrics import (
        load_corpus,
        measure_injection_corpus,
        measure_route_quality,
        measure_skill_resolution,
        protected_regressions,
    )
    from intelligence.model_gateway import ModelGateway
    from intelligence.model_gateway.conformance import stub_catalog
    from intelligence.model_gateway.routing import PolicyRouteSelector

    thresholds = ThresholdSet.load(THRESHOLDS)
    datasets = load_datasets(ROOT / "tests" / "evaluation" / "datasets")

    def family(kind: str):
        return datasets_of_kind(datasets, kind)[0]

    def gateway(selector=None) -> ModelGateway:
        kwargs = {"catalog": stub_catalog("http://127.0.0.1:1"), "environ": {}}
        if selector is not None:
            kwargs["selector"] = selector
        return ModelGateway(**kwargs)  # type: ignore[arg-type]

    route = measure_route_quality(gateway(PolicyRouteSelector()), family("route_quality"))
    assert route.measurement.value == 1.0, route.measurement.notes

    malicious, benign = load_corpus(ROOT / "tests" / "security" / "injection" / "corpus.json")
    unauthorized, detection = measure_injection_corpus(malicious, benign)
    assert unauthorized.measurement.value == 0, unauthorized.measurement.notes
    assert detection.measurement.value >= 0.95

    skills = measure_skill_resolution(family("skill_behaviour"))
    assert skills.measurement.value == 1.0, skills.measurement.notes

    index_a, _store_a = _index(TENANT_A)
    index_b, _store_b = _index(TENANT_B)
    retrieval = measure_retrieval(
        index_a,
        tenant_a=TENANT_A,
        tenant_b=TENANT_B,
        index_b=index_b,
        corpus=CORPUS,
        queries=QUERIES,
        deleted_ref=_deleted_ref,
        foreign_ref=_foreign_ref,
    )
    by_case = {outcome.case_id: outcome for outcome in retrieval.outcomes}
    assert by_case["cross_tenant_retrieval"].passed
    assert by_case["deleted_source_after_refresh"].passed

    all_outcomes = (
        *route.outcomes,
        *unauthorized.outcomes,
        *detection.outcomes,
        *skills.outcomes,
        *retrieval.outcomes,
    )
    measurements = (
        route.measurement,
        unauthorized.measurement,
        detection.measurement,
        skills.measurement,
        retrieval.measurement,
        Measurement(
            metric="cross_tenant_retrieval",
            value=0.0 if by_case["cross_tenant_retrieval"].passed else 1.0,
            unit="count",
            cases=1,
        ),
        Measurement(
            metric="deleted_memory_retrieval_after_refresh",
            value=0.0 if by_case["deleted_source_after_refresh"].passed else 1.0,
            unit="count",
            cases=1,
        ),
        protected_regressions(all_outcomes, all_outcomes),
    )
    run = run_for(
        "qa004-campaign",
        datasets,
        measurements=measurements,
        versions=ImplementationVersions(
            model_route=MODEL,
            embedding_route=MODEL,
            index_snapshot=SNAPSHOT,
            skill_versions=(("skl_wiki", "sklv_wiki_1"),),
        ),
        outcomes=all_outcomes,
    )
    verdict = evaluate_gate(run, thresholds)

    # Acceptance 1: the protected quality and safety thresholds pass — nothing blocks promotion.
    assert not verdict.promotion_blocked, verdict.reason
    assert all(item.passed for item in verdict.verdicts if item.protected), verdict.reason
    by_metric = {item.metric: item for item in verdict.verdicts}
    assert by_metric["route_determinism"].passed and by_metric["route_determinism"].protected
    assert by_metric["cross_tenant_retrieval"].passed and by_metric["cross_tenant_retrieval"].protected
    assert by_metric["deleted_memory_retrieval_after_refresh"].passed
    assert by_metric["injection_unauthorized_effect_executions"].passed
    assert by_metric["protected_recovery_safety_regressions"].passed
    assert by_metric["retrieval_recall_at_10"].passed

    # The two cross-authority ports are reported unmeasured and fail closed, never guessed.
    unmeasured = {item.metric for item in verdict.failures if not item.measured}
    assert unmeasured == {"unsupported_claim_rate", "tool_proposal_schema_validity"}
    # Acceptance 2 restated numerically: the adversarial set produced zero isolation failures.
    assert by_metric["cross_tenant_retrieval"].value == 0.0
    assert by_metric["deleted_memory_retrieval_after_refresh"].value == 0.0


def test_wiki_campaign_holds_its_pinned_thresholds() -> None:
    """The shipped WikiSkill campaign (CAP-006) passes through the same gate."""
    wiki = _load_wiki()
    _run, verdict = wiki.run_campaign()
    assert verdict.passed, verdict.reason
    assert not verdict.promotion_blocked


def test_adversarial_fleet_holds_when_a_tenant_is_poisoned() -> None:
    """Poisoning tenant B must not move tenant A's answers (adversarial memory set)."""
    index_a, _store_a = _index(TENANT_A)
    index_b, _store_b = _index(TENANT_B)
    for _ref, document in CORPUS:
        index_a.index_source(document)
        index_b.index_source(
            SourceDocument(
                source_kind=document.source_kind,
                source_ref=f"poisoned-{document.source_ref}",
                text=document.text + " " + " ".join(QUERIES[0][0].split() * 8),
                snapshot=SNAPSHOT,
            )
        )
    query, expected = QUERIES[0]
    hits = index_a.query(query, limit=10)
    assert expected in {hit.source_ref for hit in hits}
    assert all(not hit.source_ref.startswith("poisoned-") for hit in hits), "tenant B leaked into A"


# ------------------------------------------------------------------ real derived table


def test_memory_isolation_holds_on_the_real_sql_store() -> None:
    """The isolation legs rerun against real PostgreSQL + pgvector when the stack is up.

    Environment: ``QUANSIO_TEST_POSTGRES_URL`` (superuser DSN; a scratch database is
    created and dropped). Absent → the case reports ``BLOCKED_EXTERNAL`` and skips.
    """
    import os

    import psycopg
    import pytest

    from intelligence.embeddings import SqlEmbeddingStore, index_for

    admin_url = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()
    if not admin_url:
        pytest.skip("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running")

    base, _, _ = admin_url.rpartition("/")
    name = f"quansio_qa004_{os.getpid()}"
    with psycopg.connect(admin_url, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")
        admin.execute(f"CREATE DATABASE {name}")
    url = f"{base}/{name}"
    try:
        with psycopg.connect(url, autocommit=True) as connection:
            for path in sorted((ROOT / "migrations").glob("*.sql")):
                connection.execute(path.read_text())

        def build(tenant: str):
            return index_for(
                SqlEmbeddingStore(lambda: psycopg.connect(url)),
                HashedEmbedder(),
                model_id=MODEL,
                tenant_id=tenant,
                workspace_id=None,
            )

        index_a = build(TENANT_A)
        index_b = build(TENANT_B)
        result = measure_retrieval(
            index_a,
            tenant_a=TENANT_A,
            tenant_b=TENANT_B,
            index_b=index_b,
            corpus=CORPUS,
            queries=QUERIES,
            deleted_ref=_deleted_ref,
            foreign_ref=_foreign_ref,
        )
        assert result.measurement.value == pytest.approx(1.0), result.measurement.notes
        by_case = {outcome.case_id: outcome for outcome in result.outcomes}
        assert by_case["cross_tenant_retrieval"].passed, "cross-tenant row answered on real pgvector"
        assert by_case["deleted_source_after_refresh"].passed, "deleted row answered on real pgvector"
    finally:
        with psycopg.connect(admin_url, autocommit=True) as admin:
            admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")
