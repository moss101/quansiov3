"""The derived embedding index: chunking, embedding, pgvector storage and semantic search (INT-011).

The index is *derived*: ``derived.embeddings`` can be dropped and rebuilt from the
authoritative rows and object storage, and it is never a source of truth (DOSSIER.md §9).
Three properties are structural rather than conventional:

* **A tenant scope is intrinsic.** An [`EmbeddingIndex`] is constructed *for* one tenant and
  carries it privately; no public method accepts a tenant, every SQL statement in
  [`SqlEmbeddingStore`] filters on it, and the derived table's row-level security is forced
  as well. Cross-tenant retrieval is therefore impossible by construction rather than by
  remembering to add a predicate.
* **Rows are keyed by provenance.** A row carries source kind, source ref, snapshot, content
  digest and chunk index, so a rebuild can prove it reproduced the same content, an unchanged
  chunk is never re-embedded, and a deleted source disappears by deleting its key.
* **Nothing is inferred from text.** Similarity is the provider's vector distance; the index
  never parses a chunk to decide what it is.
"""

from __future__ import annotations

import contextlib
from collections.abc import Callable, Iterator, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Protocol

from intelligence.embeddings.chunking import Chunk, chunk_text
from intelligence.embeddings.provider import EmbeddingError, EmbeddingProvider
from intelligence.model_gateway.ids import new_ulid

#: Refusal rules, named so a caller can tell which one fired.
RULE_TENANT_REQUIRED = "index.tenant_required"
RULE_SOURCE_SHAPE = "index.source_shape"
RULE_QUERY_SHAPE = "index.query_shape"
RULE_DIMENSIONS = "index.dimensions"
RULE_STORE = "index.store"

#: The width `derived.embeddings` pins (``migrations/``); a mismatch is refused, never cast.
INDEX_DIMENSIONS = 1_536
#: The derived table's own identity prefix: `migrations/` constrains the column with
#: `CHECK (id LIKE 'emb\_%')`, so the schema is the authority for the shape.
ROW_ID_PREFIX = "emb_"
#: Hits a query returns when the caller does not ask for a bound.
DEFAULT_LIMIT = 10
#: Hard ceiling on hits, so one query cannot drain the vector index into memory.
MAX_LIMIT = 100

#: SQL statements owned by this module. Every one of them filters on the tenant: the derived
#: table carries `tenant_id` and is under forced row-level security, and this module adds the
#: explicit predicate so a missing context cannot become a cross-tenant read even if a policy
#: were ever dropped. A structural test pins that invariant.
_UPSERT_SQL = """
INSERT INTO derived.embeddings
    (id, tenant_id, workspace_id, source_kind, source_ref, model_id, dimensions, embedding,
     snapshot, content_digest, chunk_index)
VALUES
    (%(id)s, %(tenant_id)s, %(workspace_id)s, %(source_kind)s, %(source_ref)s, %(model_id)s,
     %(dimensions)s, %(embedding)s::vector, %(snapshot)s, %(content_digest)s, %(chunk_index)s)
ON CONFLICT (tenant_id, source_kind, source_ref, model_id, snapshot, content_digest, chunk_index)
DO UPDATE SET embedding = EXCLUDED.embedding, workspace_id = EXCLUDED.workspace_id,
              updated_at = now()
"""

_DIGESTS_SQL = """
SELECT chunk_index, content_digest FROM derived.embeddings
WHERE tenant_id = %(tenant_id)s AND source_kind = %(source_kind)s
  AND source_ref = %(source_ref)s AND model_id = %(model_id)s AND snapshot = %(snapshot)s
"""

_DELETE_SOURCE_SQL = """
DELETE FROM derived.embeddings
WHERE tenant_id = %(tenant_id)s AND source_kind = %(source_kind)s
  AND source_ref = %(source_ref)s
"""

_DELETE_SUPERSEDED_SQL = """
DELETE FROM derived.embeddings
WHERE tenant_id = %(tenant_id)s AND source_kind = %(source_kind)s
  AND source_ref = %(source_ref)s AND model_id = %(model_id)s AND snapshot <> %(snapshot)s
"""

_DELETE_MISSING_SQL = """
DELETE FROM derived.embeddings
WHERE tenant_id = %(tenant_id)s AND source_kind = %(source_kind)s
  AND source_ref = %(source_ref)s AND model_id = %(model_id)s AND snapshot = %(snapshot)s
  AND content_digest <> ALL(%(keep)s)
"""

_SEARCH_SQL = """
SELECT source_kind, source_ref, model_id, snapshot, content_digest, chunk_index,
       embedding <=> %(query)s::vector AS distance
FROM derived.embeddings
WHERE tenant_id = %(tenant_id)s
  AND (workspace_id IS NOT DISTINCT FROM %(workspace_id)s::text)
  AND (%(snapshot)s::text IS NULL OR snapshot = %(snapshot)s::text)
ORDER BY distance, source_kind, source_ref, chunk_index
LIMIT %(limit)s
"""

_COUNT_SQL = "SELECT count(*) FROM derived.embeddings WHERE tenant_id = %(tenant_id)s"

_SOURCES_SQL = """
SELECT DISTINCT source_ref FROM derived.embeddings
WHERE tenant_id = %(tenant_id)s AND source_kind = %(source_kind)s
"""

_PRUNE_SQL = """
DELETE FROM derived.embeddings
WHERE tenant_id = %(tenant_id)s AND source_kind = %(source_kind)s
  AND source_ref <> ALL(%(keep)s::text[])
"""

_TENANT_CONTEXT_SQL = "SELECT set_config('quansio.tenant_id', %(tenant_id)s, true)"

#: Every statement this module owns, for the tenant-filter invariant.
STATEMENTS: tuple[str, ...] = (
    _UPSERT_SQL,
    _DIGESTS_SQL,
    _DELETE_SOURCE_SQL,
    _DELETE_SUPERSEDED_SQL,
    _DELETE_MISSING_SQL,
    _SEARCH_SQL,
    _COUNT_SQL,
    _SOURCES_SQL,
    _PRUNE_SQL,
)


class EmbeddingIndexError(RuntimeError):
    """A derived-index operation that cannot proceed (never a partial or widened read)."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class EmbeddingRow:
    """One derived row: a chunk's vector, keyed by where the chunk came from."""

    id: str
    tenant_id: str
    workspace_id: str | None
    source_kind: str
    source_ref: str
    model_id: str
    dimensions: int
    embedding: tuple[float, ...]
    snapshot: str
    content_digest: str
    chunk_index: int


@dataclass(frozen=True, slots=True)
class StoredEmbedding:
    """One search hit: the row's key and its distance from the query vector."""

    source_kind: str
    source_ref: str
    model_id: str
    snapshot: str
    content_digest: str
    chunk_index: int
    distance: float


@dataclass(frozen=True, slots=True)
class SourceDocument:
    """A source to index: its kind, its ref, its text and the snapshot that text belongs to."""

    source_kind: str
    source_ref: str
    text: str
    snapshot: str


@dataclass(frozen=True, slots=True)
class IndexReport:
    """What indexing one source did."""

    source_kind: str
    source_ref: str
    snapshot: str
    model_id: str
    chunks: int
    embedded: int
    reused: int
    removed: int


@dataclass(frozen=True, slots=True)
class RebuildReport:
    """What a full rebuild did, per source and in total."""

    sources: tuple[IndexReport, ...]
    removed: int
    #: Rows of sources the authoritative set no longer names (deletion propagation on rebuild).
    pruned: int = 0

    @property
    def embedded(self) -> int:
        return sum(report.embedded for report in self.sources)

    @property
    def chunks(self) -> int:
        return sum(report.chunks for report in self.sources)


class EmbeddingStore(Protocol):
    """The derived storage port: real SQL in [`SqlEmbeddingStore`], a double in tests."""

    def upsert(self, rows: Sequence[EmbeddingRow]) -> int:
        """Write rows, replacing a row with the same key."""

    def digests(
        self,
        *,
        tenant_id: str,
        source_kind: str,
        source_ref: str,
        model_id: str,
        snapshot: str,
    ) -> Mapping[int, str]:
        """The content digests already indexed for one source snapshot, by chunk index."""

    def delete_source(self, *, tenant_id: str, source_kind: str, source_ref: str) -> int:
        """Delete every row of one source (deletion propagation)."""

    def delete_superseded(
        self,
        *,
        tenant_id: str,
        source_kind: str,
        source_ref: str,
        model_id: str,
        snapshot: str,
    ) -> int:
        """Delete rows of one source that belong to another snapshot."""

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
        """Delete rows of one source snapshot whose content digest is no longer current."""

    def search(
        self,
        *,
        tenant_id: str,
        workspace_id: str | None,
        query: Sequence[float],
        limit: int,
        snapshot: str | None,
    ) -> Sequence[StoredEmbedding]:
        """Nearest rows for one tenant; the tenant filter is required, never optional."""

    def count(self, *, tenant_id: str) -> int:
        """Rows this tenant has in the index."""

    def sources(self, *, tenant_id: str, source_kind: str) -> Sequence[str]:
        """The source refs of one kind this tenant currently has rows for."""

    def prune(self, *, tenant_id: str, source_kind: str, keep: frozenset[str]) -> int:
        """Delete rows of one kind whose source ref is not in ``keep``; returns rows removed."""


class SqlConnection(Protocol):
    """The slice of a DB-API connection this store uses (psycopg satisfies it)."""

    def cursor(self) -> SqlCursor:
        """Open a cursor."""

    def commit(self) -> None:
        """Commit the open transaction."""

    def rollback(self) -> None:
        """Discard the open transaction."""


class SqlCursor(Protocol):
    """The slice of a DB-API cursor this store uses."""

    @property
    def rowcount(self) -> int:
        """Rows the last statement affected."""

    def execute(self, query: str, params: Mapping[str, object] | None = None) -> object:
        """Run one parameterized statement."""

    def fetchall(self) -> Sequence[Sequence[object]]:
        """Every row of the last statement."""

    def close(self) -> None:
        """Close the cursor."""


def _vector_literal(vector: Sequence[float]) -> str:
    """The pgvector text form of a vector: ``[1.0,2.0]``."""
    return "[" + ",".join(repr(float(value)) for value in vector) + "]"


def _as_int(value: object, column: str) -> int:
    """A decoded column that must be an integer; anything else is refused, not coerced."""
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        return int(value)
    raise EmbeddingIndexError(
        "INTERNAL", RULE_STORE, f"column {column} decoded as {type(value).__name__}, expected an integer"
    )


def _as_float(value: object, column: str) -> float:
    """A decoded column that must be a number; anything else is refused, not coerced."""
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        return float(value)
    raise EmbeddingIndexError(
        "INTERNAL", RULE_STORE, f"column {column} decoded as {type(value).__name__}, expected a number"
    )


@dataclass(slots=True)
class SqlEmbeddingStore:
    """The real store: pgvector in the ``derived`` schema, one transaction per operation.

    ``connect`` returns a fresh DB-API connection; the store sets the tenant context on it
    before every statement so the table's forced row-level security has a context, and every
    statement filters on the tenant as well. A failed statement rolls its transaction back, so
    an interrupted re-index never leaves the index half-updated.
    """

    connect: Callable[[], SqlConnection]

    @contextlib.contextmanager
    def _transaction(self, tenant_id: str) -> Iterator[SqlCursor]:
        connection = self.connect()
        cursor = connection.cursor()
        try:
            cursor.execute(_TENANT_CONTEXT_SQL, {"tenant_id": tenant_id})
            yield cursor
        except Exception:
            cursor.close()
            connection.rollback()
            raise
        else:
            cursor.close()
            connection.commit()

    def upsert(self, rows: Sequence[EmbeddingRow]) -> int:
        if not rows:
            return 0
        with self._transaction(rows[0].tenant_id) as cursor:
            for row in rows:
                cursor.execute(
                    _UPSERT_SQL,
                    {
                        "id": row.id,
                        "tenant_id": row.tenant_id,
                        "workspace_id": row.workspace_id,
                        "source_kind": row.source_kind,
                        "source_ref": row.source_ref,
                        "model_id": row.model_id,
                        "dimensions": row.dimensions,
                        "embedding": _vector_literal(row.embedding),
                        "snapshot": row.snapshot,
                        "content_digest": row.content_digest,
                        "chunk_index": row.chunk_index,
                    },
                )
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
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _DIGESTS_SQL,
                {
                    "tenant_id": tenant_id,
                    "source_kind": source_kind,
                    "source_ref": source_ref,
                    "model_id": model_id,
                    "snapshot": snapshot,
                },
            )
            return {_as_int(row[0], "chunk_index"): str(row[1]) for row in cursor.fetchall()}

    def delete_source(self, *, tenant_id: str, source_kind: str, source_ref: str) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _DELETE_SOURCE_SQL,
                {"tenant_id": tenant_id, "source_kind": source_kind, "source_ref": source_ref},
            )
            return cursor.rowcount

    def delete_superseded(
        self,
        *,
        tenant_id: str,
        source_kind: str,
        source_ref: str,
        model_id: str,
        snapshot: str,
    ) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _DELETE_SUPERSEDED_SQL,
                {
                    "tenant_id": tenant_id,
                    "source_kind": source_kind,
                    "source_ref": source_ref,
                    "model_id": model_id,
                    "snapshot": snapshot,
                },
            )
            return cursor.rowcount

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
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _DELETE_MISSING_SQL,
                {
                    "tenant_id": tenant_id,
                    "source_kind": source_kind,
                    "source_ref": source_ref,
                    "model_id": model_id,
                    "snapshot": snapshot,
                    "keep": sorted(keep),
                },
            )
            return cursor.rowcount

    def search(
        self,
        *,
        tenant_id: str,
        workspace_id: str | None,
        query: Sequence[float],
        limit: int,
        snapshot: str | None,
    ) -> Sequence[StoredEmbedding]:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _SEARCH_SQL,
                {
                    "tenant_id": tenant_id,
                    "workspace_id": workspace_id,
                    "query": _vector_literal(query),
                    "limit": limit,
                    "snapshot": snapshot,
                },
            )
            return tuple(
                StoredEmbedding(
                    source_kind=str(row[0]),
                    source_ref=str(row[1]),
                    model_id=str(row[2]),
                    snapshot=str(row[3]),
                    content_digest=str(row[4]),
                    chunk_index=_as_int(row[5], "chunk_index"),
                    distance=_as_float(row[6], "distance"),
                )
                for row in cursor.fetchall()
            )

    def count(self, *, tenant_id: str) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(_COUNT_SQL, {"tenant_id": tenant_id})
            return _as_int(cursor.fetchall()[0][0], "count")

    def sources(self, *, tenant_id: str, source_kind: str) -> Sequence[str]:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(_SOURCES_SQL, {"tenant_id": tenant_id, "source_kind": source_kind})
            return tuple(str(row[0]) for row in cursor.fetchall())

    def prune(self, *, tenant_id: str, source_kind: str, keep: frozenset[str]) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _PRUNE_SQL,
                {"tenant_id": tenant_id, "source_kind": source_kind, "keep": sorted(keep)},
            )
            return cursor.rowcount


@dataclass(slots=True)
class EmbeddingIndex:
    """One tenant's view of the derived index.

    The tenant is bound here and never taken from a caller, so no call site can widen a read
    to another tenant however it is written.
    """

    store: EmbeddingStore
    embedder: EmbeddingProvider
    model_id: str
    tenant_id: str
    workspace_id: str | None = None
    chunk_chars: int = 1_200
    overlap_chars: int = 200
    indexed: list[str] = field(default_factory=list)

    def __post_init__(self) -> None:
        if not self.tenant_id.strip():
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA",
                RULE_TENANT_REQUIRED,
                "an embedding index must be bound to a tenant",
            )
        if not self.model_id.strip():
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA", RULE_SOURCE_SHAPE, "an embedding index needs its model id"
            )
        if self.embedder.dimensions != INDEX_DIMENSIONS:
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA",
                RULE_DIMENSIONS,
                f"the index pins {INDEX_DIMENSIONS} dimensions, the embedder produces "
                f"{self.embedder.dimensions}",
            )

    def index_source(self, document: SourceDocument) -> IndexReport:
        """Index (or re-index) one source, embedding only the chunks that changed.

        A chunk whose digest is already present for this source and snapshot is reused, so an
        edit re-embeds its tail rather than the whole document. Rows of the same source that
        belong to an earlier snapshot are removed, because the derived index answers for the
        current version of a source; other sources are untouched.
        """
        self._validate_source(document)
        chunks = chunk_text(document.text, chunk_chars=self.chunk_chars, overlap_chars=self.overlap_chars)
        existing = self.store.digests(
            tenant_id=self.tenant_id,
            source_kind=document.source_kind,
            source_ref=document.source_ref,
            model_id=self.model_id,
            snapshot=document.snapshot,
        )
        # Rows of another snapshot are superseded: the index answers for the version a caller
        # asked for, and keeping every version would let a retired text be retrieved.
        removed = self.store.delete_superseded(
            tenant_id=self.tenant_id,
            source_kind=document.source_kind,
            source_ref=document.source_ref,
            model_id=self.model_id,
            snapshot=document.snapshot,
        )
        # Rows of this snapshot whose text is gone (an edit that rewrote or shortened the
        # source) are dropped with it, so a stale chunk can never be retrieved.
        keep = frozenset(chunk.content_digest for chunk in chunks)
        removed += self.store.delete_missing(
            tenant_id=self.tenant_id,
            source_kind=document.source_kind,
            source_ref=document.source_ref,
            model_id=self.model_id,
            snapshot=document.snapshot,
            keep=keep,
        )
        current = set(existing.values()) & keep
        fresh = [chunk for chunk in chunks if chunk.content_digest not in current]
        rows = self._embed_chunks(document, fresh)
        self.store.upsert(rows)
        self.indexed.append(document.source_ref)
        return IndexReport(
            source_kind=document.source_kind,
            source_ref=document.source_ref,
            snapshot=document.snapshot,
            model_id=self.model_id,
            chunks=len(chunks),
            embedded=len(rows),
            reused=len(chunks) - len(fresh),
            removed=removed,
        )

    def delete_source(self, *, source_kind: str, source_ref: str) -> int:
        """Remove a deleted source from semantic retrieval (knowledge/memory/artifact delete)."""
        if not source_kind.strip() or not source_ref.strip():
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA", RULE_SOURCE_SHAPE, "a source needs a kind and a ref"
            )
        return self.store.delete_source(
            tenant_id=self.tenant_id, source_kind=source_kind, source_ref=source_ref
        )

    def rebuild(self, documents: Sequence[SourceDocument]) -> RebuildReport:
        """Rebuild this tenant's index for ``documents`` from the authoritative sources.

        Every row of every named source is deleted first, so the result depends only on the
        inputs and not on what the index happened to contain: rebuilding twice, or rebuilding
        after an incremental update, yields the same rows — which is what makes a rebuild a
        proof rather than a hope.
        """
        removed = 0
        for document in documents:
            removed += self.store.delete_source(
                tenant_id=self.tenant_id,
                source_kind=document.source_kind,
                source_ref=document.source_ref,
            )
        reports = tuple(self.index_source(document) for document in documents)
        return RebuildReport(sources=reports, removed=removed)

    def prune(self, *, source_kind: str, keep_refs: frozenset[str]) -> int:
        """Drop rows of ``source_kind`` whose source is not in the authoritative set.

        A rebuild knows the complete source set it read, so a source the set no longer names is
        a deletion that has already happened upstream: keeping its rows would let a removed
        source keep answering retrieval. Only the named kind is pruned, because a reader covers
        one authoritative plane and deleting another plane's rows would be a cross-owner act.
        """
        if not source_kind.strip():
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA", RULE_SOURCE_SHAPE, "a prune needs the source kind it covers"
            )
        return self.store.prune(tenant_id=self.tenant_id, source_kind=source_kind, keep=frozenset(keep_refs))

    def query(
        self, text: str, *, limit: int = DEFAULT_LIMIT, snapshot: str | None = None
    ) -> tuple[StoredEmbedding, ...]:
        """Semantically retrieve from this tenant's index.

        The tenant is this index's own; the query text is embedded through the same route the
        index was written with, and each hit carries the provenance (source, snapshot, digest)
        a caller needs to cite it.
        """
        if not text.strip():
            raise EmbeddingIndexError("VALIDATION_SCHEMA", RULE_QUERY_SHAPE, "a query needs text")
        if limit < 1 or limit > MAX_LIMIT:
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA",
                RULE_QUERY_SHAPE,
                f"limit must be between 1 and {MAX_LIMIT}, got {limit}",
            )
        vectors = self.embedder.embed((text,), deadline_ms=0)
        if len(vectors) != 1:
            raise EmbeddingError("VALIDATION_SCHEMA", RULE_STORE, "the embedder returned no query vector")
        return tuple(
            self.store.search(
                tenant_id=self.tenant_id,
                workspace_id=self.workspace_id,
                query=vectors[0],
                limit=limit,
                snapshot=snapshot,
            )
        )

    def _embed_chunks(self, document: SourceDocument, chunks: Sequence[Chunk]) -> tuple[EmbeddingRow, ...]:
        if not chunks:
            return ()
        vectors = self.embedder.embed(tuple(chunk.text for chunk in chunks), deadline_ms=0)
        rows: list[EmbeddingRow] = []
        for chunk, vector in zip(chunks, vectors, strict=True):
            if len(vector) != self.embedder.dimensions:
                raise EmbeddingError(
                    "VALIDATION_SCHEMA",
                    RULE_DIMENSIONS,
                    f"route returned a {len(vector)}-wide vector for chunk {chunk.index}",
                )
            rows.append(
                EmbeddingRow(
                    id=ROW_ID_PREFIX + new_ulid(seed=self._row_seed(document, chunk), timestamp_ms=0),
                    tenant_id=self.tenant_id,
                    workspace_id=self.workspace_id,
                    source_kind=document.source_kind,
                    source_ref=document.source_ref,
                    model_id=self.model_id,
                    dimensions=len(vector),
                    embedding=tuple(float(value) for value in vector),
                    snapshot=document.snapshot,
                    content_digest=chunk.content_digest,
                    chunk_index=chunk.index,
                )
            )
        return tuple(rows)

    def _row_seed(self, document: SourceDocument, chunk: Chunk) -> str:
        """The row id is a pure function of its key, so a rebuild rewrites the same rows."""
        return "|".join(
            (
                self.tenant_id,
                self.workspace_id or "",
                document.source_kind,
                document.source_ref,
                self.model_id,
                document.snapshot,
                chunk.content_digest,
                str(chunk.index),
            )
        )

    def _validate_source(self, document: SourceDocument) -> None:
        if not document.source_kind.strip() or not document.source_ref.strip():
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA", RULE_SOURCE_SHAPE, "a source needs a kind and a ref"
            )
        if not document.snapshot.strip():
            raise EmbeddingIndexError(
                "VALIDATION_SCHEMA",
                RULE_SOURCE_SHAPE,
                "a source needs the snapshot its text belongs to",
            )


def index_for(
    store: EmbeddingStore,
    embedder: EmbeddingProvider,
    *,
    model_id: str,
    tenant_id: str,
    workspace_id: str | None = None,
) -> EmbeddingIndex:
    """Bind an index to one tenant (the only way to obtain one)."""
    return EmbeddingIndex(
        store=store,
        embedder=embedder,
        model_id=model_id,
        tenant_id=tenant_id,
        workspace_id=workspace_id,
    )
