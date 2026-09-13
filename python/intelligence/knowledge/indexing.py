"""The semantic channel: knowledge retrievable by meaning, and the derived index kept in agreement.

Knowledge is retrievable through INT-011's derived vector index, and the index is *derived*: it must
agree with the fabric in both directions. One direction is the obvious one — every retrievable entry
is indexed. The other is the one that matters, and the reason this module exists rather than a
one-line "index it" call:

* **nothing that left retrieval is reachable by meaning.** A quarantined, superseded or withdrawn
  entry has no rows in `derived.embeddings`, so a semantic query cannot serve knowledge the fabric
  would refuse;
* **a query is filtered by the fabric as well.** The index is rebuildable and can lag a deletion by
  an instant, so [`retrieve`][intelligence.knowledge.indexing.retrieve] re-checks each hit against
  the entry's lifecycle instead of trusting the index alone — the second authority agreeing, not a
  second authority deciding.

Text comes from the authoritative source plane through INT-011's digest-verifying reader
([`DescribedKnowledgeText`][intelligence.knowledge.indexing.DescribedKnowledgeText]), with the
descriptors supplied by whoever owns the source metadata. This module reads nothing itself: it
decides *what* belongs in the channel and lets INT-011 own *how* it is stored and searched.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Protocol

from intelligence.embeddings.index import (
    DEFAULT_LIMIT,
    MAX_LIMIT,
    EmbeddingIndex,
    SourceDocument,
    StoredEmbedding,
)
from intelligence.embeddings.sources import ObjectSourceReader, SourceDescriptor
from intelligence.knowledge.ingestion import SOURCE_KIND_KNOWLEDGE
from intelligence.knowledge.models import KnowledgeEntry, KnowledgeError
from intelligence.knowledge.store import RULE_NOT_FOUND, KnowledgeFabric

#: Refusal rules owned by the semantic channel.
RULE_TEXT = "knowledge.text"
RULE_TENANT_SCOPE = "knowledge.index_tenant_scope"


class KnowledgeText(Protocol):
    """Where an entry's indexable text comes from; a missing text is `None`, never an empty string."""

    def text_for(self, entry: KnowledgeEntry) -> str | None: ...


@dataclass(slots=True)
class DescribedKnowledgeText:
    """An entry's text, read from the source plane through INT-011's digest-verifying reader.

    The descriptors are the caller's: artifact/object metadata belongs to the runtime (CORE-007), so
    the bytes are read here with the keys and digests that authority recorded, and a source whose
    bytes do not match its digest is refused by the reader rather than indexed.
    """

    reader: ObjectSourceReader
    descriptors: Mapping[str, Sequence[SourceDescriptor]]

    def text_for(self, entry: KnowledgeEntry) -> str | None:
        found = self.descriptors.get(entry.id)
        if not found:
            return None
        documents = self.reader.read_descriptors(found)
        text = "\n\n".join(document.text for document in documents)
        return text or None


@dataclass(frozen=True, slots=True)
class IndexSyncReport:
    """What one synchronisation did, per entry and in total."""

    #: Entries whose rows were (re)written this pass, in identity order.
    indexed: tuple[str, ...]
    #: Retrievable entries whose rows were already current.
    unchanged: tuple[str, ...]
    #: Retrievable entries whose text could not be read; their existing rows are kept.
    missing_text: tuple[str, ...]
    #: Rows removed because their entry is no longer retrievable.
    pruned: int
    #: Chunks written across the indexed entries.
    chunks: int

    @property
    def entries(self) -> int:
        return len(self.indexed) + len(self.unchanged)


@dataclass(frozen=True, slots=True)
class RetrievedKnowledge:
    """One semantic hit the fabric still considers retrievable."""

    entry: KnowledgeEntry
    distance: float
    chunk_index: int


@dataclass(slots=True)
class KnowledgeIndexer:
    """Keeps the derived index in agreement with one tenant's fabric."""

    fabric: KnowledgeFabric
    index: EmbeddingIndex
    texts: KnowledgeText
    source_kind: str = SOURCE_KIND_KNOWLEDGE

    def __post_init__(self) -> None:
        if self.index.tenant_id != self.fabric.tenant_id:
            raise KnowledgeError(
                "SCOPE_FORBIDDEN",
                RULE_TENANT_SCOPE,
                f"the index is bound to {self.index.tenant_id!r} and the fabric to "
                f"{self.fabric.tenant_id!r}; one channel serves one tenant",
            )
        if not self.source_kind.strip():
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_TEXT, "an indexer needs its source kind")

    def synchronize(self) -> IndexSyncReport:
        """Index every retrievable entry and drop the rows of every entry that is not.

        Only `active` knowledge is retrievable (DOMAIN.md §11.4), so only `active` knowledge is
        indexed. An entry whose text cannot be read keeps the rows it already has and is reported
        rather than pruned: pruning answers "this is no longer retrievable", never "the source was
        momentarily unreadable".
        """
        retrievable = self.fabric.retrievable(limit=MAX_LIMIT)
        indexed: list[str] = []
        unchanged: list[str] = []
        missing: list[str] = []
        chunks = 0
        for entry in retrievable:
            text = self.texts.text_for(entry)
            if text is None:
                missing.append(entry.id)
                continue
            report = self.index.index_source(
                SourceDocument(
                    source_kind=self.source_kind,
                    source_ref=entry.id,
                    text=text,
                    snapshot=_snapshot(entry),
                )
            )
            if report.embedded:
                indexed.append(entry.id)
                chunks += report.embedded
            else:
                unchanged.append(entry.id)
        pruned = self.index.prune(
            source_kind=self.source_kind,
            keep_refs=frozenset(entry.id for entry in retrievable),
        )
        return IndexSyncReport(
            indexed=tuple(indexed),
            unchanged=tuple(unchanged),
            missing_text=tuple(missing),
            pruned=pruned,
            chunks=chunks,
        )


def retrieve(
    fabric: KnowledgeFabric,
    index: EmbeddingIndex,
    query: str,
    *,
    limit: int = DEFAULT_LIMIT,
) -> tuple[RetrievedKnowledge, ...]:
    """Semantic retrieval over knowledge, filtered by the fabric's own lifecycle.

    The index answers with its nearest rows; each hit is then checked against the entry it names, so
    a row left behind by a lagging index (or by an interrupted deletion) is never served. A hit whose
    entry has gone, or whose entry is not retrievable, is dropped rather than returned with a caveat.
    """
    if index.tenant_id != fabric.tenant_id:
        raise KnowledgeError(
            "SCOPE_FORBIDDEN",
            RULE_TENANT_SCOPE,
            f"the index is bound to {index.tenant_id!r} and the fabric to {fabric.tenant_id!r}",
        )
    hits: tuple[StoredEmbedding, ...] = index.query(query, limit=limit)
    retrieved: list[RetrievedKnowledge] = []
    for hit in hits:
        if hit.source_kind != SOURCE_KIND_KNOWLEDGE:
            continue
        try:
            entry = fabric.get(hit.source_ref)
        except KnowledgeError as gone:
            if gone.rule_id != RULE_NOT_FOUND:
                raise
            continue
        if not entry.retrievable:
            continue
        retrieved.append(RetrievedKnowledge(entry=entry, distance=hit.distance, chunk_index=hit.chunk_index))
    return tuple(retrieved)


def _snapshot(entry: KnowledgeEntry) -> str:
    """The snapshot a hit cites: the entry's content version, so a stale row is attributable."""
    return f"v{entry.version}"
