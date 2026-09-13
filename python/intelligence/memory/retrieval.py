"""Memory retrievable by meaning, and the derived index kept in agreement (INT-007 unit 4).

Memory is retrieved through INT-011's derived vector index, and the index is *derived*: it must agree
with the fabric in both directions. One direction is the obvious one — every retrievable memory is
indexed. The other is the one that matters:

* **nothing that left retrieval is reachable by meaning.** A `candidate` was never promoted, a
  `deleted` memory is gone, and an expired one is out: none of them keeps rows in
  `derived.embeddings`, so a semantic query cannot serve what the fabric would refuse;
* **a query is filtered by the fabric as well.** The index is rebuildable and can lag a deletion for
  an instant, so [`retrieve`][intelligence.memory.retrieval.retrieve] re-checks each hit against the
  memory's lifecycle *and the clock* instead of trusting the index alone — the second authority
  agreeing, not a second authority deciding;
* **forgetting is one operation.** [`forget_memory`][intelligence.memory.retrieval.forget_memory]
  applies the model's terminal edge and then clears the memory's derived rows through INT-011's
  deletion seam, which is what makes "memory deletion prevents future retrieval after index refresh"
  (the task's acceptance statement, bounded by DOSSIER.md §21.3) a property of the shipped path.

Unlike the Knowledge Fabric, memory needs no source reader: the schema stores the content itself
(`memory_entries.content`), so the text to embed is the memory.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass

from intelligence.embeddings.chunking import content_digest
from intelligence.embeddings.index import (
    DEFAULT_LIMIT,
    MAX_LIMIT,
    EmbeddingIndex,
    SourceDocument,
    StoredEmbedding,
)
from intelligence.embeddings.sources import SourceDeletionPort
from intelligence.memory.models import MemoryEntry, MemoryEntryError, MemoryStatus, require_instant
from intelligence.memory.store import RULE_NOT_FOUND, MemoryFabric

#: The source kind memory contributes to the derived index (INT-011 keys rows by source kind and ref).
SOURCE_KIND_MEMORY = "memory_entry"

#: Refusal rules owned by the retrieval path.
RULE_TENANT_SCOPE = "memory.index_tenant_scope"


@dataclass(frozen=True, slots=True)
class IndexSyncReport:
    """What one synchronisation did."""

    #: Memories whose rows were (re)written this pass, in identity order.
    indexed: tuple[str, ...]
    #: Retrievable memories whose rows were already current.
    unchanged: tuple[str, ...]
    #: Rows removed because their memory is no longer retrievable at the instant given.
    pruned: int
    #: Chunks written across the indexed memories.
    chunks: int

    @property
    def entries(self) -> int:
        return len(self.indexed) + len(self.unchanged)


@dataclass(frozen=True, slots=True)
class RetrievedMemory:
    """One semantic hit the fabric still considers retrievable."""

    entry: MemoryEntry
    distance: float
    chunk_index: int


@dataclass(slots=True)
class MemoryIndexer:
    """Keeps the derived index in agreement with one tenant's memory."""

    fabric: MemoryFabric
    index: EmbeddingIndex
    source_kind: str = SOURCE_KIND_MEMORY

    def __post_init__(self) -> None:
        if self.index.tenant_id != self.fabric.tenant_id:
            raise MemoryEntryError(
                "SCOPE_FORBIDDEN",
                RULE_TENANT_SCOPE,
                f"the index is bound to {self.index.tenant_id!r} and the memory fabric to "
                f"{self.fabric.tenant_id!r}; one channel serves one tenant",
            )
        if not self.source_kind.strip():
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_TENANT_SCOPE, "an indexer needs its source kind")

    def synchronize(self, now: str) -> IndexSyncReport:
        """Index every memory retrievable at `now`, and drop the rows of every one that is not."""
        require_instant(now, field="now")
        retrievable = self.fabric.retrievable(now, limit=MAX_LIMIT)
        indexed: list[str] = []
        unchanged: list[str] = []
        chunks = 0
        for entry in retrievable:
            report = self.index.index_source(
                SourceDocument(
                    source_kind=self.source_kind,
                    source_ref=entry.id,
                    text=entry.content,
                    snapshot=snapshot_of(entry),
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
            pruned=pruned,
            chunks=chunks,
        )


def snapshot_of(entry: MemoryEntry) -> str:
    """What a hit cites: the digest of what the memory says, so a hit is attributable to its text."""
    return content_digest(entry.content)


def retrieve(
    fabric: MemoryFabric,
    index: EmbeddingIndex,
    query: str,
    *,
    now: str,
    limit: int = DEFAULT_LIMIT,
) -> tuple[RetrievedMemory, ...]:
    """Semantic retrieval over memory, filtered by the fabric's lifecycle and the clock.

    The index answers with its nearest rows; each hit is then checked against the memory it names, so
    a row left behind by a lagging index (or by an interrupted deletion) is never served. A hit whose
    memory has gone, was never promoted, was deleted, or has expired is dropped rather than returned
    with a caveat.
    """
    require_instant(now, field="now")
    if index.tenant_id != fabric.tenant_id:
        raise MemoryEntryError(
            "SCOPE_FORBIDDEN",
            RULE_TENANT_SCOPE,
            f"the index is bound to {index.tenant_id!r} and the memory fabric to {fabric.tenant_id!r}",
        )
    hits: Sequence[StoredEmbedding] = index.query(query, limit=limit)
    retrieved: list[RetrievedMemory] = []
    for hit in hits:
        if hit.source_kind != SOURCE_KIND_MEMORY:
            continue
        try:
            entry = fabric.get(hit.source_ref)
        except MemoryEntryError as gone:
            if gone.rule_id != RULE_NOT_FOUND:
                raise
            continue
        if not entry.is_retrievable_at(now):
            continue
        retrieved.append(RetrievedMemory(entry=entry, distance=hit.distance, chunk_index=hit.chunk_index))
    return tuple(retrieved)


def forget_memory(
    fabric: MemoryFabric,
    deletion: SourceDeletionPort,
    memory_id: str,
) -> tuple[MemoryEntry, int]:
    """Forget one memory: the model's terminal edge, then its derived rows, in that order.

    The lifecycle move is committed first and is what makes the memory unretrievable everywhere it is
    read from the fabric; the index call then removes the rows the semantic channel would otherwise
    still answer with. The index call is not wrapped: an unreachable index is reported to the caller
    rather than hidden, because it is rebuildable and a forgotten memory that was never recorded as
    forgotten is not.
    """
    entry = fabric.get(memory_id)
    if entry.status is not MemoryStatus.DELETED:
        entry = fabric.delete(memory_id)
    removed = deletion.delete_source(
        tenant_id=fabric.tenant_id, source_kind=SOURCE_KIND_MEMORY, source_ref=memory_id
    )
    return entry, removed
