"""A full rebuild from real object storage into real PostgreSQL + pgvector (INT-011).

These drive the shipped path end to end: CORE-007's artifact object layout in the dev MinIO is
listed, the authoritative bytes are fetched over a real signed HTTP GET and digest-verified,
they are embedded through the shipped gateway embedding route (a loopback conformance stub —
never a fake in the product path), written to ``derived.embeddings``, and retrieved with a real
pgvector similarity query.

The scratch database and the objects under the test tenant's prefix are created and removed by
the suite, so the shared stack is left as it was found.

Environment: ``QUANSIO_TEST_POSTGRES_URL`` is the DSN used to create a scratch database (the
Rust integration convention). Object storage is read through the ``QUANSIO_TEST_MINIO_*``
boundary variables with the dev-stack defaults; the suite writes its own fixtures with a
test-local signed request, because writing artifact bytes is the Rust authority's job and the
intelligence plane ships no write surface to borrow.
"""

from __future__ import annotations

import datetime
import hashlib
import hmac
import http.client
import os
import urllib.parse
from collections.abc import Iterator, Sequence
from pathlib import Path

import psycopg
import pytest

from intelligence.embeddings import (
    INDEX_DIMENSIONS,
    EmbeddingIndex,
    SourceDocument,
    SqlEmbeddingStore,
    StoredEmbedding,
    index_for,
)
from intelligence.embeddings.gateway_provider import gateway_embedder
from intelligence.embeddings.objectstore import S3Config, S3ObjectReader
from intelligence.embeddings.sources import (
    RULE_SOURCE_DIGEST,
    SOURCE_KIND_ARTIFACT,
    ArtifactObjectListing,
    IndexSourceDeletion,
    ObjectSourceReader,
    SourceDescriptor,
    SourceReadError,
    rebuild_from_sources,
)
from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.conformance import StubProvider, credential_environ, stub_catalog

ROOT = Path(__file__).resolve().parents[3]
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()

pytestmark = pytest.mark.skipif(
    not ADMIN_URL,
    reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running",
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0RRRRR"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0SSSSS"
RUNBOOK = (
    "Deployment runbook. "
    + "The pipeline promotes a candidate only after every gate passes. " * 40
    + "Retention is ninety days for logs."
)
MEMORY = "Memory entries are candidates until a person confirms them."
POLICY = "Workspace policy: retention is thirty days for metrics."


# --------------------------------------------------------------- real object fixtures


def _sign(secret: str, datestamp: str, region: str) -> bytes:
    key = hmac.new(("AWS4" + secret).encode(), datestamp.encode(), hashlib.sha256).digest()
    for part in (region, "s3", "aws4_request"):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    return key


class MinioFixture:
    """Signed PUT/DELETE for the objects this suite owns; the product path only ever reads."""

    def __init__(self, config: S3Config) -> None:
        self._config = config
        self._parts = urllib.parse.urlsplit(config.endpoint)

    def key(self, tenant: str, artifact_id: str, version_id: str, payload: bytes) -> str:
        return (
            f"tenants/{tenant}/artifacts/{artifact_id}/versions/{version_id}/"
            f"{hashlib.sha256(payload).hexdigest()}"
        )

    def put(self, object_key: str, payload: bytes) -> None:
        status, _body = self._request("PUT", f"{self._config.bucket}/{object_key}", payload)
        assert status in (200, 201), f"fixture PUT failed with HTTP {status}"

    def delete(self, object_key: str) -> None:
        status, _body = self._request("DELETE", f"{self._config.bucket}/{object_key}", b"")
        assert status in (200, 202, 204), f"fixture DELETE failed with HTTP {status}"

    def _request(self, method: str, resource: str, payload: bytes) -> tuple[int, bytes]:
        now = datetime.datetime.now(datetime.UTC)
        amz_date, datestamp = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
        payload_hash = hashlib.sha256(payload).hexdigest()
        host = self._config.host
        headers = {"host": host, "x-amz-content-sha256": payload_hash, "x-amz-date": amz_date}
        signed_names = sorted(headers)
        canonical_headers = "".join(f"{name}:{headers[name]}\n" for name in signed_names)
        signed_headers = ";".join(signed_names)
        canonical_uri = "/" + urllib.parse.quote(resource, safe="/-_.~")
        canonical_request = "\n".join(
            [method, canonical_uri, "", canonical_headers, signed_headers, payload_hash]
        )
        scope = f"{datestamp}/{self._config.region}/s3/aws4_request"
        string_to_sign = "\n".join(
            ["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical_request.encode()).hexdigest()]
        )
        signature = hmac.new(
            _sign(self._config.secret_key, datestamp, self._config.region),
            string_to_sign.encode(),
            hashlib.sha256,
        ).hexdigest()
        connection = http.client.HTTPConnection(self._parts.hostname, self._parts.port, timeout=15)
        try:
            connection.request(
                method,
                canonical_uri,
                body=payload,
                headers={
                    "Host": host,
                    "x-amz-content-sha256": payload_hash,
                    "x-amz-date": amz_date,
                    "Authorization": (
                        f"AWS4-HMAC-SHA256 Credential={self._config.access_key}/{scope}, "
                        f"SignedHeaders={signed_headers}, Signature={signature}"
                    ),
                },
            )
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()


def _minio_reachable(config: S3Config) -> bool:
    parts = urllib.parse.urlsplit(config.endpoint)
    try:
        connection = http.client.HTTPConnection(parts.hostname, parts.port, timeout=3)
        connection.request("GET", "/minio/health/live")
        connection.getresponse()
        connection.close()
        return True
    except OSError:
        return False


S3 = S3Config.from_env()


@pytest.fixture(scope="module")
def object_storage() -> Iterator[tuple[S3ObjectReader, MinioFixture]]:
    if not _minio_reachable(S3):
        pytest.skip(
            f"BLOCKED_EXTERNAL: object storage is unreachable at {S3.endpoint} (QUANSIO_TEST_MINIO_ENDPOINT)"
        )
    yield S3ObjectReader(config=S3), MinioFixture(S3)


def _scratch_url(name: str) -> str:
    base, _, _ = ADMIN_URL.rpartition("/")
    return f"{base}/{name}"


@pytest.fixture(scope="module")
def database() -> Iterator[str]:
    name = f"quansio_pyembed_src_{os.getpid()}"
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")
        admin.execute(f"CREATE DATABASE {name}")
    url = _scratch_url(name)
    with psycopg.connect(url, autocommit=True) as connection:
        for path in sorted((ROOT / "migrations").glob("*.sql")):
            connection.execute(path.read_text())
    try:
        yield url
    finally:
        with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
            admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


@pytest.fixture(scope="module")
def stub_gateway() -> Iterator[ModelGateway]:
    with StubProvider() as provider:
        yield ModelGateway(catalog=stub_catalog(provider.base_url), environ=credential_environ())


def build(database: str, gateway: ModelGateway, *, tenant_id: str = TENANT) -> EmbeddingIndex:
    return index_for(
        SqlEmbeddingStore(lambda: psycopg.connect(database)),
        gateway_embedder(gateway, dimensions=INDEX_DIMENSIONS),
        # The catalog route the index was written with, read from the catalog rather than
        # restated here (D-018: model identifiers live in config/).
        model_id=gateway.embedding_routes()[0],
        tenant_id=tenant_id,
        workspace_id=None,
    )


class OwnedObjects:
    """Objects this suite wrote, so teardown can remove exactly its own fixtures."""

    def __init__(self) -> None:
        self.keys: list[str] = []

    def write(self, fixture: MinioFixture, tenant: str, artifact_id: str, text: str) -> SourceDescriptor:
        payload = text.encode()
        object_key = fixture.key(tenant, artifact_id, f"artv-{artifact_id}", payload)
        fixture.put(object_key, payload)
        self.keys.append(object_key)
        return SourceDescriptor(
            source_kind=SOURCE_KIND_ARTIFACT,
            source_ref=artifact_id,
            snapshot=f"artv-{artifact_id}",
            object_key=object_key,
            content_digest=hashlib.sha256(payload).hexdigest(),
        )

    def forget(self, object_key: str) -> None:
        self.keys.remove(object_key)


@pytest.fixture
def fixtures(object_storage: tuple[S3ObjectReader, MinioFixture]) -> Iterator[OwnedObjects]:
    _reader, fixture = object_storage
    owned = OwnedObjects()
    try:
        yield owned
    finally:
        for object_key in owned.keys:
            fixture.delete(object_key)


def _scoped_reader(
    object_storage: tuple[S3ObjectReader, MinioFixture], descriptors: list[SourceDescriptor]
) -> ObjectSourceReader:
    reader, _fixture = object_storage
    return ObjectSourceReader(listing=_FixedListing(descriptors), objects=reader)


class _FixedListing:
    """The runtime-owned source set: the descriptors the authority says to index."""

    def __init__(self, descriptors: list[SourceDescriptor]) -> None:
        self._descriptors = list(descriptors)

    def list_sources(self, *, tenant_id: str) -> list[SourceDescriptor]:
        assert tenant_id
        return list(self._descriptors)


# ---------------------------------------------------------------------- the real rebuild


def test_a_rebuild_from_real_object_storage_reproduces_retrieval(
    database: str,
    stub_gateway: ModelGateway,
    object_storage: tuple[S3ObjectReader, MinioFixture],
    fixtures: OwnedObjects,
) -> None:
    _reader, fixture = object_storage
    descriptors = [
        fixtures.write(fixture, TENANT, "art-runbook", RUNBOOK),
        fixtures.write(fixture, TENANT, "art-memory", MEMORY),
    ]
    index = build(database, stub_gateway)
    for descriptor in descriptors:
        index.index_source(
            document_for(descriptor, RUNBOOK if "runbook" in descriptor.source_ref else MEMORY)
        )
    before = index.query("retention ninety days for logs", limit=5)
    assert hits_for(before, "art-runbook")

    real_objects = object_storage[0]
    report = rebuild_from_sources(index, _scoped_reader(object_storage, descriptors), tenant_id=TENANT)
    assert report.chunks > 0
    assert report.embedded == report.chunks
    assert report.pruned == 0
    assert index.query("retention ninety days for logs", limit=5) == before, (
        "a rebuild from the same authoritative bytes is not a diff"
    )

    # The listing path (CORE-007 object keys) picks up the same two sources.
    listed = ArtifactObjectListing(objects=real_objects).list_sources(tenant_id=TENANT)
    assert {descriptor.source_ref for descriptor in listed} >= {"art-runbook", "art-memory"}
    assert all(descriptor.content_digest for descriptor in listed)


def test_a_source_deleted_from_object_storage_disappears_from_real_retrieval(
    database: str,
    stub_gateway: ModelGateway,
    object_storage: tuple[S3ObjectReader, MinioFixture],
    fixtures: OwnedObjects,
) -> None:
    _reader, fixture = object_storage
    live = fixtures.write(fixture, TENANT, "art-live", RUNBOOK)
    retired = fixtures.write(fixture, TENANT, "art-retired", MEMORY)
    index = build(database, stub_gateway)
    rebuild_from_sources(index, _scoped_reader(object_storage, [live, retired]), tenant_id=TENANT)
    assert hits_for(index.query("memory entries candidates", limit=10), "art-retired")

    # The authority deleted the artifact: the object and the source set both lose it.
    fixture.delete(retired.object_key)
    fixtures.forget(retired.object_key)
    report = rebuild_from_sources(index, _scoped_reader(object_storage, [live]), tenant_id=TENANT)
    assert report.pruned > 0
    hits = index.query("memory entries candidates", limit=10)
    assert not hits_for(hits, "art-retired"), "a deleted source must not be retrievable"
    assert hits_for(hits, "art-live")


def test_corruption_in_real_object_storage_is_refused(
    database: str,
    stub_gateway: ModelGateway,
    object_storage: tuple[S3ObjectReader, MinioFixture],
    fixtures: OwnedObjects,
) -> None:
    _reader, fixture = object_storage
    descriptor = fixtures.write(fixture, TENANT, "art-corrupt", POLICY)
    index = build(database, stub_gateway)
    before = index.store.count(tenant_id=TENANT)
    # Rewriting the object's bytes under the same key is exactly the corruption case: the key's
    # digest is the authority's record, the bytes no longer match it.
    fixture.put(descriptor.object_key, b"tampered bytes")
    with pytest.raises(SourceReadError) as refusal:
        rebuild_from_sources(index, _scoped_reader(object_storage, [descriptor]), tenant_id=TENANT)
    assert refusal.value.rule_id == RULE_SOURCE_DIGEST
    assert index.store.count(tenant_id=TENANT) == before, "a refused read leaves the index as it was"


def test_the_real_index_stays_isolated_per_tenant_after_a_rebuild(
    database: str,
    stub_gateway: ModelGateway,
    object_storage: tuple[S3ObjectReader, MinioFixture],
    fixtures: OwnedObjects,
) -> None:
    _reader, fixture = object_storage
    mine = fixtures.write(fixture, TENANT, "art-mine", RUNBOOK)
    theirs = fixtures.write(fixture, TENANT_B, "art-theirs", RUNBOOK)
    index_a = build(database, stub_gateway, tenant_id=TENANT)
    index_b = build(database, stub_gateway, tenant_id=TENANT_B)
    rebuild_from_sources(index_a, _scoped_reader(object_storage, [mine]), tenant_id=TENANT)
    rebuild_from_sources(index_b, _scoped_reader(object_storage, [theirs]), tenant_id=TENANT_B)

    assert hits_for(index_a.query("retention ninety days for logs", limit=10), "art-mine")
    assert not hits_for(index_a.query("retention ninety days for logs", limit=10), "art-theirs")
    assert hits_for(index_b.query("retention ninety days for logs", limit=10), "art-theirs")

    counts = {
        ref: f"SELECT count(*) FROM derived.embeddings WHERE source_ref = '{ref}'"
        for ref in ("art-mine", "art-theirs")
    }
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SET ROLE quansio_app")
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        assert connection.execute(counts["art-mine"]).fetchone()[0] > 0
        assert connection.execute(counts["art-theirs"]).fetchone()[0] == 0, (
            "two tenants' artifact rows live in one table and must not be mutually visible"
        )
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
        for ref, query in counts.items():
            assert connection.execute(query).fetchone()[0] == 0, (
                f"a missing tenant context must fail closed, not expose {ref}"
            )


def test_the_deletion_seam_removes_real_rows(
    database: str,
    stub_gateway: ModelGateway,
    object_storage: tuple[S3ObjectReader, MinioFixture],
    fixtures: OwnedObjects,
) -> None:
    _reader, fixture = object_storage
    descriptor = fixtures.write(fixture, TENANT, "art-seam", RUNBOOK)
    index = build(database, stub_gateway)
    rebuild_from_sources(index, _scoped_reader(object_storage, [descriptor]), tenant_id=TENANT)
    assert hits_for(index.query("retention ninety days for logs", limit=10), "art-seam")

    port = IndexSourceDeletion(
        index_for_tenant=lambda tenant_id: build(database, stub_gateway, tenant_id=tenant_id)
    )
    removed = port.delete_source(tenant_id=TENANT, source_kind=SOURCE_KIND_ARTIFACT, source_ref="art-seam")
    assert removed > 0
    assert not hits_for(index.query("retention ninety days for logs", limit=10), "art-seam")


def document_for(descriptor: SourceDescriptor, text: str) -> SourceDocument:
    """The indexable document for a fixture, carrying the authority's snapshot."""
    return SourceDocument(
        source_kind=descriptor.source_kind,
        source_ref=descriptor.source_ref,
        text=text,
        snapshot=descriptor.snapshot,
    )


def hits_for(hits: Sequence[StoredEmbedding], source_ref: str) -> list[int]:
    """The chunk indexes a query retrieved for one source: a search always returns neighbours."""
    return [hit.chunk_index for hit in hits if hit.source_ref == source_ref]
