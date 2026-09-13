"""The memory channel's rules, without a database (INT-007 unit 4).

These prove the two directions of agreement the channel must hold — everything retrievable is indexed,
and nothing that left retrieval is reachable by meaning — plus the checks that keep them true: an
expired memory is pruned because retrieval may not serve it, the snapshot is the digest of the text, a
mismatched tenant is refused, and retrieval re-checks every hit against the fabric and the clock
instead of trusting the index. The database-backed suite proves the same on real rows and pgvector.
"""

from __future__ import annotations

from collections.abc import Sequence

import pytest
from test_memory_store_boundaries import TENANT, RecordingStore, entry

from intelligence.embeddings.chunking import content_digest
from intelligence.embeddings.index import (
    EmbeddingIndexError,
    IndexReport,
    RebuildReport,
    SourceDocument,
    StoredEmbedding,
)
from intelligence.memory.models import MemoryEntryError, MemoryStatus
from intelligence.memory.retrieval import (
    SOURCE_KIND_MEMORY,
    MemoryIndexer,
    retrieve,
    snapshot_of,
)
from intelligence.memory.store import memory_for

OTHER_TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ"
NOW = "2026-09-13T12:00:00Z"


class RecordingIndex:
    """The INT-011 index as the channel uses it, recording what it was asked to store."""

    def __init__(
        self,
        *,
        tenant_id: str = TENANT,
        hits: Sequence[StoredEmbedding] = (),
        embedded: int = 1,
    ) -> None:
        self.tenant_id = tenant_id
        self.workspace_id = None
        self.hits = tuple(hits)
        self.embedded = embedded
        self.indexed: list[SourceDocument] = []
        self.pruned: list[tuple[str, frozenset[str]]] = []
        self.queries: list[str] = []

    def index_source(self, document: SourceDocument) -> IndexReport:
        self.indexed.append(document)
        return IndexReport(
            source_kind=document.source_kind,
            source_ref=document.source_ref,
            snapshot=document.snapshot,
            model_id="embedding-route",
            chunks=max(self.embedded, 1),
            embedded=self.embedded,
            reused=0,
            removed=0,
        )

    def prune(self, *, source_kind: str, keep_refs: frozenset[str]) -> int:
        self.pruned.append((source_kind, keep_refs))
        return 2

    def query(
        self, text: str, *, limit: int = 10, snapshot: str | None = None
    ) -> tuple[StoredEmbedding, ...]:
        self.queries.append(text)
        return self.hits

    def rebuild(self, documents: Sequence[SourceDocument]) -> RebuildReport:  # pragma: no cover
        raise AssertionError("the channel synchronises incrementally, it does not rebuild")


def channel(rows: Sequence[object], **index_overrides: object):
    bound = memory_for(RecordingStore(rows=rows), tenant_id=TENANT)  # type: ignore[arg-type]
    index = RecordingIndex(**index_overrides)  # type: ignore[arg-type]
    return MemoryIndexer(fabric=bound, index=index), index, bound


def hit(source_ref: str, *, distance: float = 0.1, source_kind: str = SOURCE_KIND_MEMORY):
    return StoredEmbedding(
        source_kind=source_kind,
        source_ref=source_ref,
        model_id="embedding-route",
        snapshot="v1",
        content_digest="f" * 64,
        chunk_index=0,
        distance=distance,
    )


# ------------------------------------------------------------------- what gets indexed


def test_only_retrievable_memory_is_indexed() -> None:
    rows = [
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", status=MemoryStatus.CANDIDATE),
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0BBBBB", status=MemoryStatus.DELETED),
        entry(
            id="mem_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
            status=MemoryStatus.ACTIVE,
            expires_at="2026-01-01T00:00:00Z",
        ),
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0DDDDD", status=MemoryStatus.ACTIVE),
    ]
    channel_, index, _fabric = channel(rows)
    report = channel_.synchronize(NOW)

    kept = "mem_01J8Z3K6F1N8VQ2X5W9Y0DDDDD"
    assert report.indexed == (kept,)
    assert [document.source_ref for document in index.indexed] == [kept]
    assert index.pruned == [(SOURCE_KIND_MEMORY, frozenset({kept}))], (
        "a candidate, a deleted memory and an expired one all leave the channel"
    )


def test_a_hit_cites_the_memory_and_the_digest_of_what_it_says() -> None:
    item = entry(content="Prefers concise summaries.", status=MemoryStatus.ACTIVE)
    channel_, index, _fabric = channel([item])
    channel_.synchronize(NOW)
    document = index.indexed[0]
    assert document.source_kind == SOURCE_KIND_MEMORY
    assert document.source_ref == item.id
    assert document.text == item.content, "memory stores its text, so there is nothing to fetch"
    assert document.snapshot == content_digest(item.content)
    assert snapshot_of(item) == content_digest(item.content)
    assert len(snapshot_of(item)) == 64


def test_re_synchronising_reuses_what_is_already_current() -> None:
    item = entry(status=MemoryStatus.ACTIVE)
    channel_, _index, _fabric = channel([item], embedded=0)
    report = channel_.synchronize(NOW)
    assert report.indexed == ()
    assert report.unchanged == (item.id,)
    assert report.chunks == 0
    assert report.entries == 1


def test_the_channel_refuses_a_mismatched_tenant() -> None:
    bound = memory_for(RecordingStore(), tenant_id=TENANT)
    with pytest.raises(MemoryEntryError) as refusal:
        MemoryIndexer(fabric=bound, index=RecordingIndex(tenant_id=OTHER_TENANT))  # type: ignore[arg-type]
    assert refusal.value.code == "SCOPE_FORBIDDEN"
    assert refusal.value.rule_id == "memory.index_tenant_scope"


def test_the_channel_needs_a_source_kind() -> None:
    bound = memory_for(RecordingStore(), tenant_id=TENANT)
    with pytest.raises(MemoryEntryError) as refusal:
        MemoryIndexer(fabric=bound, index=RecordingIndex(), source_kind="  ")  # type: ignore[arg-type]
    assert refusal.value.rule_id == "memory.index_tenant_scope"


# -------------------------------------------------------------------------- retrieval


def test_retrieval_returns_active_memory_by_meaning() -> None:
    item = entry(status=MemoryStatus.ACTIVE)
    bound = memory_for(RecordingStore(rows=[item]), tenant_id=TENANT)
    index = RecordingIndex(hits=(hit(item.id),))
    results = retrieve(bound, index, "concise", now=NOW)
    assert [result.entry.id for result in results] == [item.id]
    assert results[0].distance == 0.1
    assert results[0].chunk_index == 0
    assert results[0].entry.content == item.content
    assert index.queries == ["concise"]


def test_retrieval_drops_hits_the_fabric_would_refuse() -> None:
    for status, expires in (
        (MemoryStatus.CANDIDATE, ""),
        (MemoryStatus.DELETED, ""),
        (MemoryStatus.ACTIVE, "2026-01-01T00:00:00Z"),
    ):
        item = entry(status=status, expires_at=expires)
        bound = memory_for(RecordingStore(rows=[item]), tenant_id=TENANT)
        index = RecordingIndex(hits=(hit(item.id),))
        assert retrieve(bound, index, "concise", now=NOW) == (), (status.value, expires)


def test_retrieval_drops_a_hit_whose_memory_is_gone() -> None:
    bound = memory_for(RecordingStore(), tenant_id=TENANT)
    index = RecordingIndex(hits=(hit("mem_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ"),))
    assert retrieve(bound, index, "concise", now=NOW) == ()


def test_retrieval_ignores_hits_from_another_channel() -> None:
    item = entry(status=MemoryStatus.ACTIVE)
    bound = memory_for(RecordingStore(rows=[item]), tenant_id=TENANT)
    index = RecordingIndex(hits=(hit(item.id, source_kind="knowledge_entry"), hit(item.id)))
    assert [result.entry.id for result in retrieve(bound, index, "concise", now=NOW)] == [item.id]


def test_retrieval_refuses_a_mismatched_tenant_or_a_bad_instant() -> None:
    bound = memory_for(RecordingStore(), tenant_id=TENANT)
    with pytest.raises(MemoryEntryError) as refusal:
        retrieve(bound, RecordingIndex(tenant_id=OTHER_TENANT), "concise", now=NOW)  # type: ignore[arg-type]
    assert refusal.value.code == "SCOPE_FORBIDDEN"
    with pytest.raises(MemoryEntryError) as instant:
        retrieve(bound, RecordingIndex(), "concise", now="2026-09-13 12:00:00")  # type: ignore[arg-type]
    assert instant.value.rule_id == "memory.instant"


def test_a_broken_index_surfaces_as_itself() -> None:
    class BrokenIndex(RecordingIndex):
        def index_source(self, document: SourceDocument) -> IndexReport:
            raise EmbeddingIndexError("INTERNAL", "index.store", "the derived table is unreachable")

    item = entry(status=MemoryStatus.ACTIVE)
    bound = memory_for(RecordingStore(rows=[item]), tenant_id=TENANT)
    with pytest.raises(EmbeddingIndexError) as failure:
        MemoryIndexer(fabric=bound, index=BrokenIndex()).synchronize(NOW)  # type: ignore[arg-type]
    assert failure.value.rule_id == "index.store"


def test_memory_needs_no_source_reader_because_the_schema_stores_the_text() -> None:
    """A structural check: unlike knowledge, the channel reads the memory itself, not object storage."""
    import inspect

    from intelligence.memory import retrieval

    source = inspect.getsource(retrieval)
    assert "ObjectSourceReader" not in source
    assert "S3ObjectReader" not in source
    assert "content_digest" in source
