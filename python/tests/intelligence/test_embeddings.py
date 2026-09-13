"""Embedding pipeline and derived index tests (INT-011).

These exercise the pipeline's rules without a provider or a database: a deterministic
in-memory store stands in for `derived.embeddings` and a deterministic fake embedder stands in
for the gateway, so each property under test — rebuild equivalence, deletion propagation, the
mandatory tenant filter, incremental re-embedding and route failover — is asserted on the
rules rather than on a wire.

The fake vector space is a bag of hashed words, so assertions are about *which* source a
query retrieves (the hits are distance-ordered) and about exactly which rows exist, rather
than about an absolute similarity: a one-word query against a long chunk has a low cosine by
construction, which says nothing about retrieval correctness.
"""

from __future__ import annotations

import hashlib
import math
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field

import pytest

from intelligence.embeddings import (
    DEFAULT_LIMIT,
    INDEX_DIMENSIONS,
    STATEMENTS,
    ChunkingError,
    EmbeddingError,
    EmbeddingIndex,
    EmbeddingIndexError,
    EmbeddingRow,
    FailoverEmbedder,
    SourceDocument,
    StoredEmbedding,
    chunk_text,
    content_digest,
    index_for,
)

TENANT_A = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
MODEL = "embedding-route"


# --------------------------------------------------------------- deterministic fakes


@dataclass
class FakeEmbedder:
    """A route whose vectors are a bag of hashed words, so shared words mean closeness."""

    route_id: str = MODEL
    dimensions: int = INDEX_DIMENSIONS
    failing: bool = False
    calls: list[tuple[str, ...]] = field(default_factory=list)

    def embed(self, texts: Sequence[str], *, deadline_ms: int) -> tuple[tuple[float, ...], ...]:
        self.calls.append(tuple(texts))
        if self.failing:
            raise RuntimeError("the provider refused the request")
        return tuple(self._vector(text) for text in texts)

    def _vector(self, text: str) -> tuple[float, ...]:
        buckets = [0.0] * self.dimensions
        for word in text.lower().split():
            digest = hashlib.sha256(word.encode("utf-8")).digest()
            buckets[int.from_bytes(digest[:8], "big") % self.dimensions] += 1.0
        norm = math.sqrt(sum(value * value for value in buckets)) or 1.0
        return tuple(value / norm for value in buckets)


@dataclass
class MemoryStore:
    """The derived-table contract in memory, including the tenant predicate on every read."""

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
        self,
        *,
        tenant_id: str,
        source_kind: str,
        source_ref: str,
        model_id: str,
        snapshot: str,
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
        self,
        *,
        tenant_id: str,
        source_kind: str,
        source_ref: str,
        model_id: str,
        snapshot: str,
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
        hits: list[StoredEmbedding] = []
        for row in self.rows.values():
            if row.tenant_id != tenant_id or row.workspace_id != workspace_id:
                continue
            if snapshot is not None and row.snapshot != snapshot:
                continue
            hits.append(
                StoredEmbedding(
                    source_kind=row.source_kind,
                    source_ref=row.source_ref,
                    model_id=row.model_id,
                    snapshot=row.snapshot,
                    content_digest=row.content_digest,
                    chunk_index=row.chunk_index,
                    distance=_cosine_distance(query, row.embedding),
                )
            )
        hits.sort(key=lambda hit: (hit.distance, hit.source_kind, hit.source_ref, hit.chunk_index))
        return tuple(hits[:limit])

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


def _cosine_distance(left: Sequence[float], right: Sequence[float]) -> float:
    dot = sum(a * b for a, b in zip(left, right, strict=True))
    left_norm = math.sqrt(sum(value * value for value in left)) or 1.0
    right_norm = math.sqrt(sum(value * value for value in right)) or 1.0
    return 1.0 - dot / (left_norm * right_norm)


def runner(retention: str = "Retention is ninety days for logs.") -> str:
    """A document long enough to need several chunks, whose tail a test can extend."""
    return (
        "Deployment runbook. "
        + "The pipeline promotes a candidate only after every gate passes. " * 40
        + retention
    )


def build_index(
    store: MemoryStore | None = None,
    embedder: FakeEmbedder | FailoverEmbedder | None = None,
    *,
    tenant_id: str = TENANT_A,
    workspace_id: str | None = WORKSPACE,
) -> EmbeddingIndex:
    return index_for(
        store if store is not None else MemoryStore(),
        embedder if embedder is not None else FakeEmbedder(),
        model_id=MODEL,
        tenant_id=tenant_id,
        workspace_id=workspace_id,
    )


def document(
    text: str = "deployment runbook", *, source_ref: str = "knowledge-1", snapshot: str = "rev-1"
) -> SourceDocument:
    return SourceDocument(source_kind="knowledge_entry", source_ref=source_ref, text=text, snapshot=snapshot)


# --------------------------------------------------------------------------- chunking


def test_chunking_is_deterministic_and_keyed_by_content() -> None:
    first = chunk_text(runner())
    second = chunk_text(runner())
    assert first == second, "the same text must chunk identically"
    assert len(first) > 1, "the fixture must need more than one chunk"
    assert [chunk.index for chunk in first] == list(range(len(first)))
    assert all(chunk.content_digest == content_digest(chunk.text) for chunk in first)

    extended = chunk_text(runner() + " A new paragraph closes the runbook.")
    shared = min(len(first) - 1, len(extended) - 1)
    assert shared > 0
    assert [chunk.content_digest for chunk in extended][:shared] == [chunk.content_digest for chunk in first][
        :shared
    ], "appending must leave the earlier chunks unchanged"


def test_chunking_refuses_what_it_cannot_mean() -> None:
    with pytest.raises(ChunkingError) as empty:
        chunk_text("   \n\n  ")
    assert empty.value.rule_id == "chunk.empty"
    assert empty.value.code == "VALIDATION_SCHEMA"

    with pytest.raises(ChunkingError) as window:
        chunk_text("text", chunk_chars=8)
    assert window.value.rule_id == "chunk.parameters"

    with pytest.raises(ChunkingError) as overlap:
        chunk_text("text", chunk_chars=400, overlap_chars=400)
    assert overlap.value.rule_id == "chunk.parameters"


# ------------------------------------------------------------------ rebuild equivalence


def test_a_rebuild_reproduces_identical_retrieval() -> None:
    store = MemoryStore()
    index = build_index(store)
    documents = [
        document(runner(), source_ref="knowledge-1"),
        document("Memory entries are candidates until a person confirms them.", source_ref="memory-1"),
    ]
    for entry in documents:
        index.index_source(entry)
    first = index.query("retention ninety days for logs", limit=5)
    assert first

    report = index.rebuild(documents)
    assert report.chunks == sum(len(chunk_text(entry.text)) for entry in documents)
    assert report.embedded == report.chunks, "a rebuild after a drop re-embeds everything"
    second = index.query("retention ninety days for logs", limit=5)
    assert second == first, "a rebuild from the same sources is not a diff"

    rows_before = dict(store.rows)
    index.rebuild(documents)
    assert dict(store.rows) == rows_before, "a second rebuild rewrites the same rows"


def test_rebuilding_after_an_edit_matches_a_fresh_index() -> None:
    edited = document(runner() + " Retention is thirty days for metrics.", snapshot="rev-2")
    incremental_store, fresh_store = MemoryStore(), MemoryStore()
    incremental = build_index(incremental_store)
    incremental.index_source(document(runner()))
    incremental.index_source(edited)

    fresh = build_index(fresh_store)
    fresh.rebuild([edited])

    assert incremental.query("retention thirty days for metrics", limit=5) == fresh.query(
        "retention thirty days for metrics", limit=5
    )


# ------------------------------------------------------------------- incremental work


def test_an_edit_re_embeds_only_what_changed() -> None:
    store = MemoryStore()
    embedder = FakeEmbedder()
    index = build_index(store, embedder)
    index.index_source(document(runner()))
    embedder.calls.clear()

    report = index.index_source(document(runner() + " A new paragraph closes the runbook.", snapshot="rev-1"))
    assert report.reused > 0, "unchanged chunks are not re-embedded"
    assert report.embedded < report.chunks
    embedded_texts = [text for call in embedder.calls for text in call]
    assert len(embedded_texts) == report.embedded


def test_a_superseded_snapshot_leaves_retrieval() -> None:
    index = build_index()
    index.index_source(document(runner(), snapshot="rev-1"))
    assert index.query("retention", limit=5, snapshot="rev-1")

    index.index_source(document(runner() + " Revised.", snapshot="rev-2"))
    assert index.query("retention", limit=5, snapshot="rev-1") == (), (
        "the superseded snapshot's rows are gone"
    )
    assert index.query("retention", limit=5, snapshot="rev-2")


# ------------------------------------------------------------ deletion propagation


def test_deleting_a_source_removes_it_from_retrieval() -> None:
    store = MemoryStore()
    index = build_index(store)
    index.index_source(document(runner(), source_ref="knowledge-1"))
    index.index_source(document("Memory entries are candidates.", source_ref="memory-1"))
    assert index.query("retention ninety days for logs", limit=5)[0].source_ref == "knowledge-1"

    removed = index.delete_source(source_kind="knowledge_entry", source_ref="knowledge-1")
    assert removed > 0
    assert all(
        hit.source_ref != "knowledge-1" for hit in index.query("retention ninety days for logs", limit=5)
    ), "a deleted source has no rows left to retrieve"
    assert index.query("memory entries candidates", limit=5), "other sources are untouched"
    assert store.count(tenant_id=TENANT_A) > 0


# ------------------------------------------------------------------- tenant isolation


def test_one_index_can_only_ever_see_its_own_tenant() -> None:
    store = MemoryStore()
    index_a = build_index(store, tenant_id=TENANT_A)
    index_b = build_index(store, tenant_id=TENANT_B)

    index_a.index_source(document("Retention is ninety days for logs.", source_ref="shared"))
    index_b.index_source(document("Retention is ninety days for logs.", source_ref="shared"))

    hits_a = index_a.query("retention ninety days for logs", limit=10)
    hits_b = index_b.query("retention ninety days for logs", limit=10)
    assert len(hits_a) == len(hits_b) == 1, "each index sees exactly its own row"
    assert {row.tenant_id for row in store.rows.values()} == {TENANT_A, TENANT_B}

    index_a.delete_source(source_kind="knowledge_entry", source_ref="shared")
    assert index_a.query("retention", limit=10) == ()
    assert index_b.query("retention ninety days for logs", limit=10), (
        "one tenant's delete cannot remove another tenant's rows"
    )


def test_every_statement_is_tenant_scoped() -> None:
    assert STATEMENTS, "the module owns statements to check"
    for statement in STATEMENTS:
        assert "%(tenant_id)s" in statement, statement
    writes = [statement for statement in STATEMENTS if statement.strip().startswith("INSERT")]
    assert len(writes) == 1, "one insert, which stamps the tenant on every row"
    for statement in STATEMENTS:
        if statement in writes:
            assert "tenant_id," in statement
            continue
        assert "tenant_id = %(tenant_id)s" in statement, statement


def test_the_query_api_takes_no_tenant() -> None:
    index = build_index()
    with pytest.raises(TypeError):
        index.query("retention", tenant_id=TENANT_B)  # type: ignore[call-arg]


def test_an_index_must_be_bound_to_a_tenant() -> None:
    with pytest.raises(EmbeddingIndexError) as refusal:
        build_index(tenant_id="  ")
    assert refusal.value.rule_id == "index.tenant_required"


# ------------------------------------------------------------------------ failover


def test_an_embedding_route_fails_over_and_records_every_attempt() -> None:
    primary = FakeEmbedder(route_id="primary", failing=True)
    secondary = FakeEmbedder(route_id="secondary")
    embedder = FailoverEmbedder(providers=(primary, secondary), dimensions=INDEX_DIMENSIONS)
    index = build_index(embedder=embedder)

    index.index_source(document(runner()))
    assert index.query("retention", limit=3)
    assert [attempt.route_id for attempt in embedder.attempts] == ["primary", "secondary"]
    assert [attempt.outcome for attempt in embedder.attempts] == ["failed", "answered"]
    assert secondary.calls

    total = FailoverEmbedder(providers=(primary,), dimensions=INDEX_DIMENSIONS)
    with pytest.raises(EmbeddingError) as unavailable:
        build_index(embedder=total).index_source(document(runner()))
    assert unavailable.value.code == "PROVIDER_UNAVAILABLE"
    assert unavailable.value.rule_id == "embedding.provider_failed"
    assert "primary" in unavailable.value.detail


def test_a_route_of_the_wrong_width_is_refused_before_any_call() -> None:
    wide = FakeEmbedder(route_id="other-space", dimensions=768)
    with pytest.raises(EmbeddingError) as mismatch:
        FailoverEmbedder(providers=(wide,), dimensions=INDEX_DIMENSIONS)
    assert mismatch.value.rule_id == "embedding.dimension_mismatch"
    assert wide.calls == [], "no request was made"

    with pytest.raises(EmbeddingError) as empty:
        FailoverEmbedder(providers=(), dimensions=INDEX_DIMENSIONS)
    assert empty.value.rule_id == "embedding.no_providers"


def test_a_malformed_vector_set_is_refused_rather_than_indexed() -> None:
    class Truncating(FakeEmbedder):
        def embed(self, texts: Sequence[str], *, deadline_ms: int) -> tuple[tuple[float, ...], ...]:
            return super().embed(texts, deadline_ms=deadline_ms)[:-1]

    store = MemoryStore()
    embedder = FailoverEmbedder(
        providers=(Truncating(route_id="truncating"), FakeEmbedder(route_id="honest")),
        dimensions=INDEX_DIMENSIONS,
    )
    index = build_index(store, embedder)
    index.index_source(document(runner()))
    assert store.count(tenant_id=TENANT_A) > 0, "the honest route indexed the source"
    assert embedder.attempts[0].outcome == "failed"
    assert embedder.attempts[-1].route_id == "honest"


# --------------------------------------------------------------------- query bounds


def test_the_query_bounds_are_enforced() -> None:
    index = build_index()
    index.index_source(document(runner()))
    with pytest.raises(EmbeddingIndexError) as blank:
        index.query("   ")
    assert blank.value.rule_id == "index.query_shape"
    with pytest.raises(EmbeddingIndexError):
        index.query("retention", limit=0)
    with pytest.raises(EmbeddingIndexError):
        index.query("retention", limit=10_000)
    assert index.query("retention", limit=DEFAULT_LIMIT)
