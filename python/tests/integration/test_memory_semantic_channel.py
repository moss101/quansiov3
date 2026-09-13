"""Memory retrievable by meaning, against real PostgreSQL and the real derived index (INT-007 unit 4).

The two acceptance statements this closes are here: a runtime restart must succeed with memory
*disabled* — which the model proves structurally, since nothing in this plane is recovery state — and
**a deletion must stop future retrieval once the derived index has refreshed**, which is what these
tests drive end to end: propose, promote, index, forget, and check that both planes agree afterwards.

The index is INT-011's, exercised with the shipped gateway embedder over a loopback conformance stub,
so the embedding path is the product's; the fabric is the real `public.memory_entries` store.

Environment: ``QUANSIO_TEST_POSTGRES_URL`` is the superuser DSN used to create a scratch database.
Absent → the suite reports ``BLOCKED_EXTERNAL`` and skips.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

import psycopg
import pytest

from intelligence.embeddings import INDEX_DIMENSIONS, SqlEmbeddingStore, index_for
from intelligence.embeddings.gateway_provider import gateway_embedder
from intelligence.embeddings.sources import IndexSourceDeletion
from intelligence.memory.candidates import MemoryCandidate, propose
from intelligence.memory.models import MemoryProvenance, MemoryStatus
from intelligence.memory.retrieval import (
    SOURCE_KIND_MEMORY,
    MemoryIndexer,
    forget_memory,
    retrieve,
)
from intelligence.memory.store import SqlMemoryStore, memory_for
from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import StubProvider, credential_environ, stub_catalog

ROOT = Path(__file__).resolve().parents[3]
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()
SEEDER = ROOT / "scripts" / "dev" / "seed_test_database.py"

pytestmark = pytest.mark.skipif(
    not ADMIN_URL,
    reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running",
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
SUBJECT = "usr_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
NOW = "2026-09-13T12:00:00Z"
PREFERENCE = "Prefers concise summaries and always summarizes retention policy decisions."
OTHER = "Wants every deployment runbook reviewed before a release is promoted."


def seed_scratch_database(name: str, *, tenants: list[str], workspaces: list[str]) -> dict:
    result = subprocess.run(
        [
            sys.executable,
            str(SEEDER),
            "--admin-url",
            ADMIN_URL,
            "--database",
            name,
            *(f"--tenant={tenant}" for tenant in tenants),
            *(f"--workspace={workspace}" for workspace in workspaces),
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(f"seeding {name} failed:\n{result.stdout}\n{result.stderr}")
    return json.loads(result.stdout.strip().splitlines()[-1])


def drop_scratch_database(name: str) -> None:
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


@pytest.fixture(scope="module")
def database() -> Iterator[str]:
    name = f"quansio_pymemr_{os.getpid()}"
    seeded = seed_scratch_database(name, tenants=[TENANT], workspaces=[WORKSPACE])
    try:
        yield seeded["url"]
    finally:
        drop_scratch_database(name)


@pytest.fixture(autouse=True)
def empty_state(database: str) -> Iterator[None]:
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("DELETE FROM memory_entries")
        connection.execute("DELETE FROM derived.embeddings")
    yield


@pytest.fixture(scope="module")
def stub_gateway() -> Iterator[ModelGateway]:
    with StubProvider() as provider:
        yield ModelGateway(catalog=stub_catalog(provider.base_url), environ=credential_environ())


def build_index(database: str, gateway: ModelGateway):
    return index_for(
        SqlEmbeddingStore(lambda: psycopg.connect(database)),
        gateway_embedder(gateway, dimensions=INDEX_DIMENSIONS),
        model_id=gateway.embedding_routes()[0],
        tenant_id=TENANT,
        workspace_id=None,
    )


@pytest.fixture
def fabric(database: str):
    return memory_for(SqlMemoryStore(lambda: psycopg.connect(database)), tenant_id=TENANT)


@pytest.fixture
def index(database: str, stub_gateway: ModelGateway):
    return build_index(database, stub_gateway)


@pytest.fixture
def deletion(database: str, stub_gateway: ModelGateway) -> IndexSourceDeletion:
    return IndexSourceDeletion(index_for_tenant=lambda tenant_id: build_index(database, stub_gateway))


def remember(fabric, content: str, *, expires_at: str = "") -> str:
    outcome = propose(
        fabric,
        MemoryCandidate(
            subject_ref=SUBJECT,
            content=content,
            provenance_kind=MemoryProvenance.EXPLICIT_USER,
            provenance_ref="msg_01J8Z3K6F1N8VQ2X5W9Y0MMMMM",
            expires_at=expires_at,
        ),
        workspace_id=WORKSPACE,
    )
    fabric.set_status(outcome.entry.id, MemoryStatus.ACTIVE)
    return outcome.entry.id


def test_promoted_memory_is_retrievable_by_meaning(fabric, index) -> None:
    memory_id = remember(fabric, PREFERENCE)
    channel = MemoryIndexer(fabric=fabric, index=index)
    report = channel.synchronize(NOW)
    assert report.indexed == (memory_id,)
    assert report.chunks >= 1
    assert report.pruned == 0

    found = retrieve(fabric, index, "concise summaries retention policy", now=NOW, limit=5)
    assert [item.entry.id for item in found] == [memory_id] * len(found)
    stored = index.query("concise summaries retention policy", limit=1)[0]
    assert stored.source_kind == SOURCE_KIND_MEMORY
    assert stored.source_ref == memory_id
    assert stored.snapshot, "a hit cites the digest of the text it answered with"


def test_a_candidate_is_never_indexed(fabric, index) -> None:
    outcome = propose(
        fabric,
        MemoryCandidate(
            subject_ref=SUBJECT,
            content=PREFERENCE,
            provenance_kind=MemoryProvenance.EXPLICIT_USER,
        ),
        workspace_id=WORKSPACE,
    )
    channel = MemoryIndexer(fabric=fabric, index=index)
    assert channel.synchronize(NOW).indexed == ()
    assert retrieve(fabric, index, "concise summaries", now=NOW) == ()
    assert fabric.get(outcome.entry.id).status is MemoryStatus.CANDIDATE


def test_deleting_a_memory_removes_it_from_both_planes(fabric, index, deletion) -> None:
    """The acceptance statement: deletion prevents future retrieval after the index refreshes."""
    memory_id = remember(fabric, PREFERENCE)
    other_id = remember(fabric, OTHER)
    channel = MemoryIndexer(fabric=fabric, index=index)
    assert channel.synchronize(NOW).entries == 2
    assert retrieve(fabric, index, "concise summaries retention policy", now=NOW, limit=10)

    deleted, removed = forget_memory(fabric, deletion, memory_id)
    assert deleted.status is MemoryStatus.DELETED
    assert removed > 0, "the memory's derived rows are gone with it"
    assert fabric.get(memory_id).status is MemoryStatus.DELETED
    assert fabric.count() == 2, "the forgotten memory keeps its row for audit"

    hits = index.query("concise summaries retention policy", limit=10)
    assert not [hit for hit in hits if hit.source_ref == memory_id]
    found = retrieve(fabric, index, "concise summaries retention policy", now=NOW, limit=10)
    assert not [item for item in found if item.entry.id == memory_id]
    assert [item.entry.id for item in found] == [other_id] * len(found), "the unrelated memory still answers"


def test_an_expired_memory_is_pruned_from_the_channel(fabric, index) -> None:
    memory_id = remember(fabric, PREFERENCE, expires_at="2026-01-01T00:00:00Z")
    channel = MemoryIndexer(fabric=fabric, index=index)
    assert channel.synchronize("2025-12-01T00:00:00Z").indexed == (memory_id,)
    assert retrieve(fabric, index, "concise summaries", now="2025-12-01T00:00:00Z")

    report = channel.synchronize(NOW)
    assert report.entries == 0, "an expired memory is not retrievable, so it is not indexed"
    assert report.pruned > 0
    assert index.query("concise summaries", limit=5) == ()
    assert fabric.get(memory_id).status is MemoryStatus.ACTIVE, "an expiry is not a deletion"


def test_a_stale_index_row_cannot_be_served(fabric, index) -> None:
    """Retrieval re-checks the fabric and the clock, so a row the index still holds is not memory."""
    memory_id = remember(fabric, PREFERENCE)
    MemoryIndexer(fabric=fabric, index=index).synchronize(NOW)
    assert index.query("concise summaries retention policy", limit=5), "the row is still there"

    fabric.delete(memory_id)
    assert index.query("concise summaries retention policy", limit=5), "the index has not refreshed"
    assert retrieve(fabric, index, "concise summaries retention policy", now=NOW) == ()


def test_synchronising_again_reuses_current_rows(fabric, index) -> None:
    remember(fabric, PREFERENCE)
    channel = MemoryIndexer(fabric=fabric, index=index)
    assert channel.synchronize(NOW).indexed
    again = channel.synchronize(NOW)
    assert again.indexed == ()
    assert again.entries == 1
    assert again.chunks == 0
    assert again.pruned == 0
