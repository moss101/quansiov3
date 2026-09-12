"""Embedding pipeline and derived vector index maintenance in pgvector; the index is rebuildable (INT-011).

The index lives in the ``derived`` schema and is never a source of truth: rows are keyed by
provenance (source kind, source ref, snapshot, content digest, chunk index), a rebuild from the
authoritative sources reproduces the same retrieval, a deleted source disappears by deleting
its key, and every statement is scoped to the tenant the index was bound to.
"""

from intelligence.embeddings.chunking import (
    DEFAULT_CHUNK_CHARS,
    DEFAULT_OVERLAP_CHARS,
    Chunk,
    ChunkingError,
    chunk_text,
    content_digest,
)
from intelligence.embeddings.index import (
    DEFAULT_LIMIT,
    INDEX_DIMENSIONS,
    MAX_LIMIT,
    ROW_ID_PREFIX,
    STATEMENTS,
    EmbeddingIndex,
    EmbeddingIndexError,
    EmbeddingRow,
    EmbeddingStore,
    IndexReport,
    RebuildReport,
    SourceDocument,
    SqlEmbeddingStore,
    StoredEmbedding,
    index_for,
)
from intelligence.embeddings.provider import (
    Attempt,
    EmbeddingError,
    EmbeddingProvider,
    FailoverEmbedder,
)

__all__ = [
    "DEFAULT_CHUNK_CHARS",
    "DEFAULT_LIMIT",
    "DEFAULT_OVERLAP_CHARS",
    "INDEX_DIMENSIONS",
    "MAX_LIMIT",
    "ROW_ID_PREFIX",
    "STATEMENTS",
    "Attempt",
    "Chunk",
    "ChunkingError",
    "EmbeddingError",
    "EmbeddingIndex",
    "EmbeddingIndexError",
    "EmbeddingProvider",
    "EmbeddingRow",
    "EmbeddingStore",
    "FailoverEmbedder",
    "IndexReport",
    "RebuildReport",
    "SourceDocument",
    "SqlEmbeddingStore",
    "StoredEmbedding",
    "chunk_text",
    "content_digest",
    "index_for",
]
