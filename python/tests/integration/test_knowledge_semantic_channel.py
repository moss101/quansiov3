"""Knowledge retrievable by meaning, against real PostgreSQL and the real derived index (INT-006 unit 4).

The unit tests prove the channel's rules; these prove the rows. Knowledge is ingested into the
canonical table, its text is indexed through INT-011 (the shipped gateway embedder over a loopback
stub, so the embedding path is the product's), and retrieval is then exercised through pgvector — for
active knowledge, and after a source is quarantined or an entry withdrawn, when the answer must stop
coming back in both planes.

The text source is `DescribedKnowledgeText`, the shipped binding over INT-011's digest-verifying
object reader; the object bytes here are an in-memory port, because INT-011's own database-backed
suite already proves that reader against real MinIO object storage.

Environment: ``QUANSIO_TEST_POSTGRES_URL`` is the superuser DSN used to create a scratch database.
Absent → the suite reports ``BLOCKED_EXTERNAL`` and skips.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections.abc import Iterator, Sequence
from pathlib import Path

import psycopg
import pytest

from intelligence.embeddings import INDEX_DIMENSIONS, SqlEmbeddingStore, index_for
from intelligence.embeddings.gateway_provider import gateway_embedder
from intelligence.embeddings.sources import (
    SOURCE_KIND_ARTIFACT,
    IndexSourceDeletion,
    ObjectSourceReader,
    SourceDescriptor,
)
from intelligence.knowledge.indexing import DescribedKnowledgeText, KnowledgeIndexer, retrieve
from intelligence.knowledge.ingestion import KnowledgeProposal, forget_entry, forget_source, ingest
from intelligence.knowledge.models import KnowledgeStatus
from intelligence.knowledge.store import SqlKnowledgeStore, knowledge_for
from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import StubProvider, credential_environ, stub_catalog

ROOT = Path(__file__).resolve().parents[3]
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()
SEEDER = ROOT / "scripts" / "dev" / "seed_test_database.py"

pytestmark = pytest.mark.skipif(
    not ADMIN_URL,
    reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running",
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK"
RUNBOOK = (
    "Deployment runbook. "
    + "The pipeline promotes a candidate only after every gate passes. " * 20
    + "Retention is ninety days for logs."
)
POLICY = "Workspace policy: retention is thirty days for metrics."


def seed_scratch_database(name: str, *, tenants: list[str], workspaces: list[str]) -> dict:
    """(Re)create a scratch database through the repository's seeding tool."""
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
    name = f"quansio_pykn_sem_{os.getpid()}"
    seeded = seed_scratch_database(name, tenants=[TENANT], workspaces=[])
    try:
        yield seeded["url"]
    finally:
        drop_scratch_database(name)


@pytest.fixture(autouse=True)
def empty_state(database: str) -> Iterator[None]:
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("DELETE FROM knowledge_entries")
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
    return knowledge_for(SqlKnowledgeStore(lambda: psycopg.connect(database)), tenant_id=TENANT)


@pytest.fixture
def index(database: str, stub_gateway: ModelGateway):
    return build_index(database, stub_gateway)


@pytest.fixture
def deletion(database: str, stub_gateway: ModelGateway) -> IndexSourceDeletion:
    return IndexSourceDeletion(index_for_tenant=lambda tenant_id: build_index(database, stub_gateway))


class ObjectBytes:
    """An in-memory object-bytes port; INT-011's suite proves the real MinIO reader."""

    def __init__(self, objects: dict[str, bytes]) -> None:
        self.objects = dict(objects)

    def read(self, object_key: str, *, limit: int) -> bytes:
        payload = self.objects[object_key]
        return payload[: limit + 1]


class FixedListing:
    def __init__(self, descriptors: Sequence[SourceDescriptor]) -> None:
        self.descriptors = tuple(descriptors)

    def list_sources(self, *, tenant_id: str) -> Sequence[SourceDescriptor]:
        return self.descriptors


def text_source(descriptors: list[SourceDescriptor], objects: dict[str, bytes]) -> DescribedKnowledgeText:
    return DescribedKnowledgeText(
        reader=ObjectSourceReader(listing=FixedListing(descriptors), objects=ObjectBytes(objects)),
        descriptors={descriptor.source_ref: [descriptor] for descriptor in descriptors},
    )


def source_descriptor(entry_id_value: str, text: str, *, key: str) -> SourceDescriptor:
    import hashlib

    payload = text.encode()
    objects_digest = hashlib.sha256(payload).hexdigest()
    return SourceDescriptor(
        source_kind=SOURCE_KIND_ARTIFACT,
        source_ref=entry_id_value,
        snapshot="v1",
        object_key=key,
        content_digest=objects_digest,
    )


def activate(fabric, entry_id_value: str):
    fabric.set_status(entry_id_value, KnowledgeStatus.VERIFIED)
    return fabric.set_status(entry_id_value, KnowledgeStatus.ACTIVE)


def test_active_knowledge_is_retrievable_by_meaning(fabric, index) -> None:
    result = ingest(
        fabric,
        KnowledgeProposal(
            kind="runbook",
            source_kind=SOURCE_KIND_ARTIFACT,
            ref="art-A",
            content_ref="obj://a",
        ),
    )
    active = activate(fabric, result.entry.id)
    descriptor = source_descriptor(active.id, RUNBOOK, key="tenants/t/artifacts/a")
    channel = KnowledgeIndexer(
        fabric=fabric,
        index=index,
        texts=text_source([descriptor], {"tenants/t/artifacts/a": RUNBOOK.encode()}),
    )
    report = channel.synchronize()
    assert report.indexed == (active.id,)
    assert report.chunks > 1
    assert report.pruned == 0

    found = retrieve(fabric, index, "retention ninety days for logs", limit=5)
    assert [item.entry.id for item in found] == [active.id] * len(found)
    assert found[0].entry.status is KnowledgeStatus.ACTIVE
    # The hit carries the entry's provenance and snapshot, so a caller can cite it.
    stored = index.query("retention ninety days for logs", limit=1)[0]
    assert stored.snapshot == f"v{active.version}"
    assert stored.model_id == index.model_id


def test_knowledge_that_left_retrieval_stops_answering_semantic_queries(
    fabric, index, deletion, database: str
) -> None:
    """The whole vertical: index knowledge, quarantine its source, and agree in both planes."""
    first = ingest(
        fabric,
        KnowledgeProposal(
            kind="runbook", source_kind=SOURCE_KIND_ARTIFACT, ref="art-A", content_ref="obj://a"
        ),
    )
    second = ingest(
        fabric,
        KnowledgeProposal(
            kind="policy", source_kind=SOURCE_KIND_ARTIFACT, ref="art-A", content_ref="obj://a2"
        ),
    )
    unrelated = ingest(
        fabric,
        KnowledgeProposal(
            kind="policy", source_kind=SOURCE_KIND_ARTIFACT, ref="art-B", content_ref="obj://b"
        ),
    )
    a1, a2, b1 = (activate(fabric, item.entry.id) for item in (first, second, unrelated))
    descriptors = [
        source_descriptor(a1.id, RUNBOOK, key="obj-a1"),
        source_descriptor(a2.id, POLICY, key="obj-a2"),
        source_descriptor(b1.id, POLICY, key="obj-b1"),
    ]
    objects = {"obj-a1": RUNBOOK.encode(), "obj-a2": POLICY.encode(), "obj-b1": POLICY.encode()}
    channel = KnowledgeIndexer(fabric=fabric, index=index, texts=text_source(descriptors, objects))
    assert channel.synchronize().entries == 3
    assert retrieve(fabric, index, "retention ninety days for logs", limit=5)

    outcome = forget_source(fabric, deletion, source_kind=SOURCE_KIND_ARTIFACT, source_ref="art-A")
    assert {item.id for item in outcome.quarantined} == {a1.id, a2.id}
    assert outcome.quarantined_rows_removed > 0, "the deletion already emptied their index rows"

    # A synchronisation keeps the two planes in agreement even if a caller never deleted anything.
    report = channel.synchronize()
    assert report.missing_text == (), "the unrelated entry is still readable"
    found = retrieve(fabric, index, "retention ninety days for logs", limit=10)
    assert not [item for item in found if item.entry.id in {a1.id, a2.id}], (
        "quarantined knowledge must not be reachable by meaning"
    )
    assert [item.entry.id for item in found] == [b1.id] * len(found)
    hits = index.query("retention ninety days for logs", limit=10)
    assert not [hit for hit in hits if hit.source_ref in {a1.id, a2.id}]
    assert fabric.get(a1.id).status is KnowledgeStatus.QUARANTINED
    assert fabric.count() == 3, "nothing was deleted to make the queries stop returning it"


def test_a_withdrawn_entry_leaves_semantic_retrieval(fabric, index, deletion) -> None:
    result = ingest(
        fabric,
        KnowledgeProposal(
            kind="runbook", source_kind=SOURCE_KIND_ARTIFACT, ref="art-C", content_ref="obj://c"
        ),
    )
    active = activate(fabric, result.entry.id)
    descriptor = source_descriptor(active.id, RUNBOOK, key="obj-c")
    channel = KnowledgeIndexer(
        fabric=fabric,
        index=index,
        texts=text_source([descriptor], {"obj-c": RUNBOOK.encode()}),
    )
    channel.synchronize()
    assert retrieve(fabric, index, "retention ninety days for logs", limit=5)

    deleted, _outcome = forget_entry(fabric, deletion, active.id)
    assert deleted.status is KnowledgeStatus.DELETED
    assert retrieve(fabric, index, "retention ninety days for logs", limit=5) == ()
    assert index.query("retention ninety days for logs", limit=5) == ()
    assert fabric.count() == 1, "the withdrawn entry is retained for audit"


def test_a_stale_index_row_cannot_be_served(fabric, index) -> None:
    """Retrieval re-checks the fabric, so a row the index still holds is not knowledge."""
    result = ingest(
        fabric,
        KnowledgeProposal(
            kind="runbook", source_kind=SOURCE_KIND_ARTIFACT, ref="art-D", content_ref="obj://d"
        ),
    )
    active = activate(fabric, result.entry.id)
    descriptor = source_descriptor(active.id, RUNBOOK, key="obj-d")
    channel = KnowledgeIndexer(
        fabric=fabric,
        index=index,
        texts=text_source([descriptor], {"obj-d": RUNBOOK.encode()}),
    )
    channel.synchronize()
    assert retrieve(fabric, index, "retention ninety days for logs", limit=5)

    # Quarantine the entry in the fabric but leave the index row in place: exactly the window a
    # lagging index opens, and the one retrieval must not serve.
    fabric.set_status(active.id, KnowledgeStatus.QUARANTINED)
    assert index.query("retention ninety days for logs", limit=5), "the row is still there"
    assert retrieve(fabric, index, "retention ninety days for logs", limit=5) == ()
