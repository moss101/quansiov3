"""The semantic channel's rules, without a database (INT-006 unit 4).

These prove the two directions of agreement the channel must hold — everything retrievable is
indexed, and nothing that left retrieval is reachable by meaning — plus the checks that keep them
true: a mismatched tenant is refused, an unreadable source keeps the rows it already had rather than
being pruned, and retrieval re-checks every hit against the fabric instead of trusting the index.
The database-backed suite proves the same on real rows and the real pgvector index.
"""

from __future__ import annotations

from collections.abc import Sequence

import pytest
from test_knowledge_store_boundaries import TENANT, RecordingStore, entry, fabric

from intelligence.embeddings.index import (
    EmbeddingIndexError,
    IndexReport,
    RebuildReport,
    SourceDocument,
    StoredEmbedding,
)
from intelligence.knowledge.indexing import (
    IndexSyncReport,
    KnowledgeIndexer,
    retrieve,
)
from intelligence.knowledge.ingestion import SOURCE_KIND_KNOWLEDGE
from intelligence.knowledge.models import (
    KnowledgeEntry,
    KnowledgeError,
    KnowledgeStatus,
)

OTHER_TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ"


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

    def index_for(self, *args: object, **kwargs: object) -> None:  # pragma: no cover
        raise AssertionError


class MappingTexts:
    """A text port backed by a mapping; a missing key is an unreadable source, not empty text."""

    def __init__(self, texts: dict[str, str]) -> None:
        self.texts = texts
        self.asked: list[str] = []

    def text_for(self, entry: KnowledgeEntry) -> str | None:
        self.asked.append(entry.id)
        return self.texts.get(entry.id)


def active(**overrides: object) -> KnowledgeEntry:
    return entry(status=KnowledgeStatus.ACTIVE, **overrides)


def indexer(rows: Sequence[KnowledgeEntry], texts: dict[str, str], **index_overrides: object):
    bound, _store = fabric(RecordingStore(rows=list(rows)))
    index = RecordingIndex(**index_overrides)  # type: ignore[arg-type]
    return KnowledgeIndexer(fabric=bound, index=index, texts=MappingTexts(texts)), index


# ------------------------------------------------------------------- what gets indexed


def test_only_retrievable_knowledge_is_indexed() -> None:
    everything = [
        entry(id="kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", status=KnowledgeStatus.CANDIDATE),
        entry(id="kn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB", status=KnowledgeStatus.VERIFIED),
        entry(id="kn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", status=KnowledgeStatus.QUARANTINED),
        entry(id="kn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD", status=KnowledgeStatus.DELETED),
        entry(
            id="kn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE",
            status=KnowledgeStatus.SUPERSEDED,
            superseded_by="kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        ),
        entry(id="kn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF", status=KnowledgeStatus.ACTIVE),
    ]
    texts = {item.id: f"text for {item.id}" for item in everything}
    channel, index = indexer(everything, texts)
    report = channel.synchronize()

    assert report.indexed == ("kn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",)
    assert [document.source_ref for document in index.indexed] == ["kn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF"]
    assert index.pruned == [(SOURCE_KIND_KNOWLEDGE, frozenset({"kn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF"}))], (
        "only the retrievable entry is kept; everything that left retrieval is pruned"
    )
    assert report.pruned == 2


def test_a_hit_cites_the_entry_and_its_version() -> None:
    item = active()
    channel, index = indexer([item], {item.id: "Retention is ninety days for logs."})
    channel.synchronize()
    document = index.indexed[0]
    assert document.source_kind == SOURCE_KIND_KNOWLEDGE
    assert document.source_ref == item.id, "the entry's identity is the index key"
    assert document.snapshot == f"v{item.version}"
    assert document.text == "Retention is ninety days for logs."


def test_re_synchronising_reuses_what_is_already_current() -> None:
    item = active()
    channel, _index = indexer([item], {item.id: "text"}, embedded=0)
    report = channel.synchronize()
    assert report.indexed == ()
    assert report.unchanged == (item.id,)
    assert report.chunks == 0
    assert report.entries == 1


def test_an_unreadable_source_keeps_its_rows_and_is_reported() -> None:
    readable = active()
    unreadable = active(id="kn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB")
    channel, index = indexer([readable, unreadable], {readable.id: "text"})
    report = channel.synchronize()

    assert report.missing_text == (unreadable.id,)
    assert [document.source_ref for document in index.indexed] == [readable.id]
    assert index.pruned == [(SOURCE_KIND_KNOWLEDGE, frozenset({readable.id, unreadable.id}))], (
        "a momentarily unreadable source is not the same as one that left retrieval"
    )


def test_the_channel_refuses_a_mismatched_tenant() -> None:
    bound, _store = fabric()
    with pytest.raises(KnowledgeError) as refusal:
        KnowledgeIndexer(
            fabric=bound,
            index=RecordingIndex(tenant_id=OTHER_TENANT),  # type: ignore[arg-type]
            texts=MappingTexts({}),
        )
    assert refusal.value.code == "SCOPE_FORBIDDEN"
    assert refusal.value.rule_id == "knowledge.index_tenant_scope"


def test_an_indexer_needs_a_source_kind() -> None:
    bound, _store = fabric()
    with pytest.raises(KnowledgeError) as refusal:
        KnowledgeIndexer(
            fabric=bound,
            index=RecordingIndex(),
            texts=MappingTexts({}),
            source_kind="   ",  # type: ignore[arg-type]
        )
    assert refusal.value.rule_id == "knowledge.text"


# -------------------------------------------------------------------------- retrieval


def hit(
    source_ref: str, *, distance: float = 0.1, source_kind: str = SOURCE_KIND_KNOWLEDGE
) -> StoredEmbedding:
    return StoredEmbedding(
        source_kind=source_kind,
        source_ref=source_ref,
        model_id="embedding-route",
        snapshot="v1",
        content_digest="e" * 64,
        chunk_index=0,
        distance=distance,
    )


def test_retrieval_returns_active_knowledge_by_meaning() -> None:
    item = active()
    bound, _store = fabric(RecordingStore(rows=[item]))
    index = RecordingIndex(hits=(hit(item.id),))
    results = retrieve(bound, index, "retention ninety days", limit=5)  # type: ignore[arg-type]
    assert [result.entry.id for result in results] == [item.id]
    assert results[0].distance == 0.1
    assert results[0].chunk_index == 0
    assert index.queries == ["retention ninety days"]


def test_retrieval_drops_a_hit_the_fabric_does_not_consider_retrievable() -> None:
    """A lagging index is a lagging index: the fabric decides what may be served."""
    quarantined = entry(id="kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", status=KnowledgeStatus.QUARANTINED)
    bound, _store = fabric(RecordingStore(rows=[quarantined]))
    index = RecordingIndex(hits=(hit(quarantined.id),))
    assert retrieve(bound, index, "retention", limit=5) == ()

    superseded = entry(
        id="kn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
        status=KnowledgeStatus.SUPERSEDED,
        superseded_by="kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    )
    bound, _store = fabric(RecordingStore(rows=[superseded]))
    assert retrieve(bound, RecordingIndex(hits=(hit(superseded.id),)), "retention") == ()  # type: ignore[arg-type]


def test_retrieval_drops_a_hit_whose_entry_is_gone() -> None:
    bound, _store = fabric()
    index = RecordingIndex(hits=(hit("kn_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ"),))
    assert retrieve(bound, index, "retention") == ()  # type: ignore[arg-type]


def test_retrieval_ignores_hits_from_another_channel() -> None:
    item = active()
    bound, _store = fabric(RecordingStore(rows=[item]))
    index = RecordingIndex(hits=(hit(item.id, source_kind="artifact"), hit(item.id)))
    results = retrieve(bound, index, "retention", limit=5)  # type: ignore[arg-type]
    assert [result.entry.id for result in results] == [item.id]


def test_retrieval_refuses_a_mismatched_tenant() -> None:
    bound, _store = fabric()
    with pytest.raises(KnowledgeError) as refusal:
        retrieve(bound, RecordingIndex(tenant_id=OTHER_TENANT), "retention")  # type: ignore[arg-type]
    assert refusal.value.code == "SCOPE_FORBIDDEN"


def test_a_report_accounts_for_every_retrievable_entry() -> None:
    first, second = active(), active(id="kn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB")
    channel, _index = indexer([first, second], {first.id: "text"}, embedded=0)
    report = channel.synchronize()
    assert set(report.indexed) | set(report.unchanged) | set(report.missing_text) == {
        first.id,
        second.id,
    }, "every retrievable entry is accounted for, whatever happened to it"
    assert isinstance(report, IndexSyncReport)


def test_the_channel_never_rebuilds_a_tenant_s_whole_index() -> None:
    """Synchronisation is incremental; INT-011 owns rebuilds, and the shipped module never asks."""
    import inspect

    from intelligence.knowledge import indexing

    source = inspect.getsource(indexing)
    assert ".rebuild(" not in source, "a whole-index rebuild is INT-011's operation, not the channel's"
    assert not hasattr(KnowledgeIndexer, "rebuild")


def test_a_broken_index_surfaces_as_itself() -> None:
    """The index's own failure must reach the caller unmasked, not become a knowledge refusal."""

    class BrokenIndex(RecordingIndex):
        def index_source(self, document: SourceDocument) -> IndexReport:
            raise EmbeddingIndexError("INTERNAL", "index.store", "the derived table is unreachable")

    item = active()
    bound, _store = fabric(RecordingStore(rows=[item]))
    channel = KnowledgeIndexer(
        fabric=bound,  # type: ignore[arg-type]
        index=BrokenIndex(),  # type: ignore[arg-type]
        texts=MappingTexts({item.id: "text"}),
    )
    with pytest.raises(EmbeddingIndexError) as failure:
        channel.synchronize()
    assert failure.value.rule_id == "index.store"
