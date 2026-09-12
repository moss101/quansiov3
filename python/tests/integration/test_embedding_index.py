"""The derived embedding index against real PostgreSQL + pgvector (INT-011).

The plane's unit tests cover the pipeline's rules against an in-memory store; these cover the
SQL the pipeline actually runs: pgvector storage in the ``derived`` schema, the HNSW index,
forced row-level security, deletion propagation and rebuild equivalence on real rows.

Only the ``derived`` schema is touched, and deliberately so: the plane proposes and the Rust
runtime commits (DOSSIER.md §3), so this suite seeds no canonical row and writes no authority
table. ``derived.embeddings`` carries a tenant but no foreign key to ``tenants``, which is
what lets a derived index be rebuilt on its own.

Environment: ``QUANSIO_TEST_POSTGRES_URL`` is the superuser DSN used to create a scratch
database (the same convention as the Rust integration tests). Absent → the suite reports
``BLOCKED_EXTERNAL`` and skips, because the local development stack is not running.
"""

from __future__ import annotations

import hashlib
import math
import os
from collections.abc import Iterator, Sequence
from pathlib import Path

import psycopg
import pytest

from intelligence.embeddings import (
    INDEX_DIMENSIONS,
    EmbeddingIndex,
    FailoverEmbedder,
    SourceDocument,
    SqlEmbeddingStore,
    index_for,
)

ROOT = Path(__file__).resolve().parents[3]
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()

pytestmark = pytest.mark.skipif(
    not ADMIN_URL,
    reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running",
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0PPPPP"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ"
MODEL = "embedding-route"


class HashEmbedder:
    """A deterministic stand-in for the gateway's embedding route."""

    def __init__(self, *, route_id: str = MODEL, dimensions: int = INDEX_DIMENSIONS) -> None:
        self._route_id = route_id
        self._dimensions = dimensions
        self.calls = 0

    @property
    def route_id(self) -> str:
        return self._route_id

    @property
    def dimensions(self) -> int:
        return self._dimensions

    def embed(self, texts: Sequence[str], *, deadline_ms: int) -> tuple[tuple[float, ...], ...]:
        self.calls += 1
        return tuple(self._vector(text) for text in texts)

    def _vector(self, text: str) -> tuple[float, ...]:
        buckets = [0.0] * self._dimensions
        for word in text.lower().split():
            digest = hashlib.sha256(word.encode("utf-8")).digest()
            buckets[int.from_bytes(digest[:8], "big") % self._dimensions] += 1.0
        norm = math.sqrt(sum(value * value for value in buckets)) or 1.0
        return tuple(value / norm for value in buckets)


def _scratch_url(name: str) -> str:
    base, _, _ = ADMIN_URL.rpartition("/")
    return f"{base}/{name}"


def _apply_migrations(url: str) -> None:
    """Apply the canonical migration set the way the Rust runner does: in filename order."""
    with psycopg.connect(url, autocommit=True) as connection:
        for path in sorted((ROOT / "migrations").glob("*.sql")):
            connection.execute(path.read_text())


@pytest.fixture(scope="module")
def database() -> Iterator[str]:
    """A scratch database with the canonical schema and two seeded tenants."""
    name = f"quansio_pyembed_{os.getpid()}"
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")
        admin.execute(f"CREATE DATABASE {name}")
    url = _scratch_url(name)
    _apply_migrations(url)
    try:
        yield url
    finally:
        with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
            admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


def build(url: str, *, tenant_id: str = TENANT) -> EmbeddingIndex:
    store = SqlEmbeddingStore(lambda: psycopg.connect(url))
    return index_for(
        store,
        FailoverEmbedder(providers=(HashEmbedder(),), dimensions=INDEX_DIMENSIONS),
        model_id=MODEL,
        tenant_id=tenant_id,
        workspace_id=None,
    )


def document(text: str, *, source_ref: str = "knowledge-1", snapshot: str = "rev-1") -> SourceDocument:
    return SourceDocument(source_kind="knowledge_entry", source_ref=source_ref, text=text, snapshot=snapshot)


def long_text(marker: str = "Retention is ninety days for logs.") -> str:
    body = "The pipeline promotes a candidate after every gate passes. " * 40
    return f"Deployment runbook. {body}{marker}"


def test_the_store_writes_reads_and_deletes_real_rows(database: str) -> None:
    index = build(database)
    report = index.index_source(document(long_text()))
    assert report.chunks > 1
    assert report.embedded == report.chunks
    assert report.reused == 0

    hits = index.query("retention ninety days for logs", limit=5)
    assert hits, "the vector search returns the chunk it indexed"
    assert hits[0].source_ref == "knowledge-1"
    assert hits[0].snapshot == "rev-1"
    assert hits[0].model_id == MODEL
    assert hits[0].distance < 1.0

    again = index.index_source(document(long_text(), snapshot="rev-2"))
    assert again.embedded == again.chunks, "a new snapshot is embedded from scratch"
    assert index.query("retention", limit=5, snapshot="rev-1") == (), (
        "the superseded snapshot's rows were deleted"
    )
    assert index.query("retention", limit=5, snapshot="rev-2")

    removed = index.delete_source(source_kind="knowledge_entry", source_ref="knowledge-1")
    assert removed == again.chunks
    assert index.query("retention", limit=5, snapshot="rev-2") == ()


def test_an_unchanged_source_is_not_re_embedded(database: str) -> None:
    index = build(database)
    index.index_source(document(long_text(), source_ref="memory-1"))
    again = index.index_source(document(long_text(), source_ref="memory-1"))
    assert again.embedded == 0
    assert again.reused == again.chunks
    index.delete_source(source_kind="knowledge_entry", source_ref="memory-1")


def test_a_rebuild_on_real_rows_reproduces_the_same_retrieval(database: str) -> None:
    index = build(database)
    documents = [
        document(long_text(), source_ref="knowledge-2"),
        document("Memory entries are candidates until a person confirms them.", source_ref="memory-2"),
    ]
    for entry in documents:
        index.index_source(entry)
    first = index.query("retention ninety days for logs", limit=5)
    assert first

    index.rebuild(documents)
    assert index.query("retention ninety days for logs", limit=5) == first


def test_row_level_security_makes_a_cross_tenant_read_impossible(database: str) -> None:
    index_a = build(database, tenant_id=TENANT)
    index_b = build(database, tenant_id=TENANT_B)
    index_a.index_source(document("Retention is ninety days for logs.", source_ref="shared"))
    index_b.index_source(document("Retention is ninety days for logs.", source_ref="shared"))

    # The same source is indexed once per tenant, so each tenant's context seeing exactly one
    # row is the isolation proof: two would mean the policies let a tenant read the other's.
    # The dev superuser bypasses row-level security, so this runs as the application role the
    # migration creates, exactly as the Rust isolation tests do.
    scoped = "SELECT count(*) FROM derived.embeddings WHERE source_ref = 'shared'"
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SET ROLE quansio_app")
        for tenant in (TENANT, TENANT_B):
            connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (tenant,))
            assert connection.execute(scoped).fetchone() == (1,), tenant
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
        assert connection.execute(scoped).fetchone() == (0,), (
            "a missing tenant context must fail closed, not expose every tenant's vectors"
        )

    query = "retention ninety days for logs"

    def shared_hits(index: EmbeddingIndex) -> list[str]:
        return [hit.source_ref for hit in index.query(query, limit=10) if hit.source_ref == "shared"]

    assert shared_hits(index_a) == ["shared"]
    assert shared_hits(index_b) == ["shared"]

    index_a.delete_source(source_kind="knowledge_entry", source_ref="shared")
    assert shared_hits(index_a) == []
    assert shared_hits(index_b) == ["shared"], "one tenant's delete cannot remove another tenant's rows"
