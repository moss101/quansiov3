"""Ingestion and the forgetting path against real PostgreSQL and the real derived index (INT-006 unit 3).

The plane's unit tests prove the owner's rules; these prove the rows and the seam: a proposal
becomes a `public.knowledge_entries` row, its text is indexed through INT-011, and forgetting the
source it came from quarantines the derived knowledge *and* empties both index sets, so quarantined
knowledge stops answering retrieval instead of staying reachable through the semantic channel.

Two real authorities are in play — the knowledge fabric (this task) and the derived index (INT-011,
`derived.embeddings` with pgvector) — and the embedding route is the shipped gateway embedder over a
loopback conformance stub, so the index path is the product's, not a stub's.

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

from intelligence.embeddings import (
    INDEX_DIMENSIONS,
    SourceDocument,
    SqlEmbeddingStore,
    index_for,
)
from intelligence.embeddings.gateway_provider import gateway_embedder
from intelligence.embeddings.sources import SOURCE_KIND_ARTIFACT, IndexSourceDeletion
from intelligence.knowledge.ingestion import (
    SOURCE_KIND_KNOWLEDGE,
    KnowledgeProposal,
    forget_entry,
    forget_source,
    ingest,
)
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
    """(Re)create a scratch database with the canonical schema and the identities it needs.

    Seeding canonical identities is tooling's job (`scripts/dev/seed_test_database.py`, the
    counterpart of `scripts/dev/seed` for the dev stack), so neither this suite nor any product
    module contains that write: a Python test may reference an identity the control plane created,
    never create one.
    """
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
    """Remove the scratch database this suite created."""
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


def _scratch_url(name: str) -> str:
    base, _, _ = ADMIN_URL.rpartition("/")
    return f"{base}/{name}"


@pytest.fixture(scope="module")
def database() -> Iterator[str]:
    name = f"quansio_pykn_ing_{os.getpid()}"
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


@pytest.fixture
def fabric(database: str):
    return knowledge_for(SqlKnowledgeStore(lambda: psycopg.connect(database)), tenant_id=TENANT)


@pytest.fixture
def index(database: str, stub_gateway: ModelGateway):
    return index_for(
        SqlEmbeddingStore(lambda: psycopg.connect(database)),
        gateway_embedder(stub_gateway, dimensions=INDEX_DIMENSIONS),
        model_id=stub_gateway.embedding_routes()[0],
        tenant_id=TENANT,
        workspace_id=None,
    )


@pytest.fixture
def deletion(database: str, stub_gateway: ModelGateway) -> IndexSourceDeletion:
    return IndexSourceDeletion(
        index_for_tenant=lambda tenant_id: index_for(
            SqlEmbeddingStore(lambda: psycopg.connect(database)),
            gateway_embedder(stub_gateway, dimensions=INDEX_DIMENSIONS),
            model_id=stub_gateway.embedding_routes()[0],
            tenant_id=tenant_id,
            workspace_id=None,
        )
    )


def index_entry(index, entry, text: str) -> None:
    """Index one knowledge entry's text the way the semantic channel will (INT-011 keying)."""
    index.index_source(
        SourceDocument(
            source_kind=SOURCE_KIND_KNOWLEDGE,
            source_ref=entry.id,
            text=text,
            snapshot=f"v{entry.version}",
        )
    )


def activate(fabric, entry):
    fabric.set_status(entry.id, KnowledgeStatus.VERIFIED)
    return fabric.set_status(entry.id, KnowledgeStatus.ACTIVE)


def test_ingested_knowledge_is_stored_and_retrievable_through_the_index(fabric, index, database: str) -> None:
    proposal = KnowledgeProposal(
        kind="runbook",
        source_kind=SOURCE_KIND_ARTIFACT,
        ref="art-A",
        digest="d" * 64,
        content_ref="obj://tenants/tn/artifacts/art-A",
        confidence=0.9,
    )
    result = ingest(fabric, proposal)
    assert result.created
    stored = result.entry
    assert stored.status is KnowledgeStatus.CANDIDATE
    assert fabric.retrievable() == ()

    active = activate(fabric, stored)
    assert [item.id for item in fabric.retrievable()] == [stored.id]
    index_entry(index, active, RUNBOOK)
    hits = index.query("retention ninety days for logs", limit=5)
    assert [hit.source_ref for hit in hits if hit.source_ref == stored.id]

    # The row is in the canonical table with its provenance, and the index row carries the entry's
    # identity as its source ref.
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        row = connection.execute(
            "SELECT provenance, status, confidence FROM knowledge_entries WHERE id = %s",
            (stored.id,),
        ).fetchone()
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
    assert row is not None
    provenance, status, confidence = row
    assert status == "active" and confidence == 0.9
    assert provenance[0]["source_kind"] == SOURCE_KIND_ARTIFACT and provenance[0]["ref"] == "art-A"


def test_forgetting_a_source_quarantines_it_and_empties_both_index_sets(
    fabric, index, deletion, database: str
) -> None:
    """The whole vertical: two sources, one deleted, and retrieval must agree with the fabric."""
    from_a = ingest(
        fabric,
        KnowledgeProposal(
            kind="runbook", source_kind=SOURCE_KIND_ARTIFACT, ref="art-A", content_ref="obj://a"
        ),
    )
    from_a_second = ingest(
        fabric,
        KnowledgeProposal(
            kind="policy", source_kind=SOURCE_KIND_ARTIFACT, ref="art-A", content_ref="obj://a2"
        ),
    )
    from_b = ingest(
        fabric,
        KnowledgeProposal(
            kind="policy", source_kind=SOURCE_KIND_ARTIFACT, ref="art-B", content_ref="obj://b"
        ),
    )
    first, second, third = (activate(fabric, item.entry) for item in (from_a, from_a_second, from_b))
    index_entry(index, first, RUNBOOK)
    index_entry(index, second, POLICY)
    index_entry(index, third, POLICY)
    # The deleted artifact's own text is indexed too, under the artifact plane's key.
    index.index_source(
        SourceDocument(source_kind=SOURCE_KIND_ARTIFACT, source_ref="art-A", text=RUNBOOK, snapshot="v1")
    )
    assert index.query("retention ninety days for logs", limit=10)

    outcome = forget_source(fabric, deletion, source_kind=SOURCE_KIND_ARTIFACT, source_ref="art-A")

    assert {item.id for item in outcome.quarantined} == {first.id, second.id}
    assert outcome.unaffected == ()
    assert outcome.index_rows_removed > 0, "the artifact's own rows left the index"
    assert outcome.quarantined_rows_removed > 0, "the quarantined entries' rows left the index"
    assert fabric.get(first.id).status is KnowledgeStatus.QUARANTINED
    assert fabric.get(second.id).status is KnowledgeStatus.QUARANTINED
    assert fabric.get(third.id).status is KnowledgeStatus.ACTIVE
    assert [item.id for item in fabric.retrievable()] == [third.id]

    hits = index.query("retention ninety days for logs", limit=10)
    assert not [hit for hit in hits if hit.source_ref in {first.id, second.id, "art-A"}], (
        "quarantined knowledge and the deleted artifact must not answer retrieval"
    )
    assert [hit for hit in hits if hit.source_ref == third.id], "the unrelated source still answers"
    assert fabric.count() == 3, "nothing was deleted to hide the quarantine"


def test_forgetting_an_entry_withdraws_it_and_empties_its_index_rows(
    fabric, index, deletion, database: str
) -> None:
    result = ingest(
        fabric,
        KnowledgeProposal(
            kind="runbook", source_kind=SOURCE_KIND_ARTIFACT, ref="art-C", content_ref="obj://c"
        ),
    )
    active = activate(fabric, result.entry)
    index_entry(index, active, RUNBOOK)
    assert [
        hit for hit in index.query("retention ninety days for logs", limit=5) if hit.source_ref == active.id
    ]

    deleted, outcome = forget_entry(fabric, deletion, active.id)
    assert deleted.status is KnowledgeStatus.DELETED
    assert outcome.index_rows_removed > 0
    assert not [
        hit for hit in index.query("retention ninety days for logs", limit=5) if hit.source_ref == active.id
    ]
    assert fabric.retrievable() == ()
    assert fabric.count() == 1, "the withdrawn entry is retained for audit"


def test_a_deletion_that_matches_nothing_leaves_the_fabric_alone(fabric, index, deletion) -> None:
    result = ingest(
        fabric,
        KnowledgeProposal(
            kind="runbook", source_kind=SOURCE_KIND_ARTIFACT, ref="art-D", content_ref="obj://d"
        ),
    )
    activate(fabric, result.entry)
    outcome = forget_source(fabric, deletion, source_kind=SOURCE_KIND_ARTIFACT, source_ref="art-nothing")
    assert outcome.quarantined == ()
    assert not outcome.changed
    assert fabric.get(result.entry.id).status is KnowledgeStatus.ACTIVE
