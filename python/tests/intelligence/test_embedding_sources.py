"""The authoritative source plane and the deletion seam (INT-011).

These drive the shipped reader, the shipped rebuild and the shipped deletion port against
deterministic fakes for the two boundaries they consume (the listing and object bytes), so each
property is asserted on behaviour rather than on a wire: a rebuild from the authoritative set
reproduces retrieval, a source the set no longer names disappears, a corrupted object is
refused, and an artifact that cannot be a text source is skipped *visibly*. The database-backed
suite proves the same operations on real rows and real object storage.
"""

from __future__ import annotations

import hashlib
from collections.abc import Sequence

import pytest
from test_embeddings import (
    FakeEmbedder,
    MemoryStore,
    build_index,
    document,
)

from intelligence.embeddings import (
    EmbeddingError,
    EmbeddingIndex,
    FailoverEmbedder,
    SourceDocument,
    index_for,
)
from intelligence.embeddings.sources import (
    RULE_SOURCE_DIGEST,
    RULE_SOURCE_NOT_TEXT,
    RULE_SOURCE_SHAPE,
    RULE_SOURCE_TENANT,
    RULE_SOURCE_TOO_LARGE,
    SOURCE_KIND_ARTIFACT,
    ArtifactObjectListing,
    IndexSourceDeletion,
    ObjectSourceReader,
    SourceDescriptor,
    SourceReadError,
    rebuild_from_sources,
)

TENANT_A = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
RUNBOOK = "Deployment runbook. Retention is ninety days for logs."
MEMORY_TEXT = "Memory entries are candidates until a person confirms them."


class FakeObjects:
    """An object store in memory; `read` honours the caller's bound like the real reader does."""

    def __init__(self, objects: dict[str, bytes]) -> None:
        self._objects = dict(objects)
        self.reads: list[str] = []

    def put(self, object_key: str, payload: bytes) -> None:
        self._objects[object_key] = payload

    def read(self, object_key: str, *, limit: int) -> bytes:
        self.reads.append(object_key)
        payload = self._objects.get(object_key)
        if payload is None:
            raise AssertionError(f"the fake store has no object {object_key!r}")
        return payload[: limit + 1]


class FakeListing:
    """A listing whose authoritative set the test controls directly."""

    def __init__(self, descriptors: Sequence[SourceDescriptor]) -> None:
        self.descriptors = tuple(descriptors)

    def list_sources(self, *, tenant_id: str) -> Sequence[SourceDescriptor]:
        assert tenant_id
        return self.descriptors


def artifact_key(tenant: str, artifact_id: str, version_id: str, payload: bytes) -> str:
    digest = hashlib.sha256(payload).hexdigest()
    return f"tenants/{tenant}/artifacts/{artifact_id}/versions/{version_id}/{digest}"


def descriptor_for(tenant: str, artifact_id: str, version_id: str, payload: bytes) -> SourceDescriptor:
    return SourceDescriptor(
        source_kind=SOURCE_KIND_ARTIFACT,
        source_ref=artifact_id,
        snapshot=version_id,
        object_key=artifact_key(tenant, artifact_id, version_id, payload),
        content_digest=hashlib.sha256(payload).hexdigest(),
    )


def build(store: MemoryStore | None = None, *, tenant_id: str = TENANT_A) -> EmbeddingIndex:
    return build_index(store, FakeEmbedder(), tenant_id=tenant_id)


def artifact_document(text: str, *, source_ref: str, snapshot: str) -> SourceDocument:
    """A source document on the artifact plane, as the reader produces them."""
    return SourceDocument(
        source_kind=SOURCE_KIND_ARTIFACT, source_ref=source_ref, text=text, snapshot=snapshot
    )


def hits_for(index: EmbeddingIndex, query: str, source_ref: str) -> list[str]:
    """The chunks of one source a query retrieves: a vector search always returns neighbours."""
    return [hit.source_ref for hit in index.query(query, limit=10) if hit.source_ref == source_ref]


# ------------------------------------------------------------------ authoritative rebuild


def test_a_rebuild_from_the_authoritative_set_reproduces_retrieval() -> None:
    tenant = TENANT_A
    runbook = RUNBOOK.encode()
    objects = FakeObjects({artifact_key(tenant, "art-runbook", "artv-1", runbook): runbook})
    index = build()
    index.index_source(artifact_document(RUNBOOK, source_ref="art-runbook", snapshot="artv-1"))
    before = index.query("retention ninety days for logs", limit=5)
    assert before

    reader = ObjectSourceReader(
        listing=FakeListing([descriptor_for(tenant, "art-runbook", "artv-1", runbook)]),
        objects=objects,
    )
    report = rebuild_from_sources(index, reader, tenant_id=tenant)
    assert report.pruned == 0
    assert report.embedded == report.chunks
    assert index.query("retention ninety days for logs", limit=5) == before
    assert objects.reads == [artifact_key(tenant, "art-runbook", "artv-1", runbook)]


def test_a_source_the_authoritative_set_no_longer_names_disappears() -> None:
    tenant = TENANT_A
    live, retired = RUNBOOK.encode(), MEMORY_TEXT.encode()
    objects = FakeObjects(
        {
            artifact_key(tenant, "art-live", "artv-1", live): live,
            artifact_key(tenant, "art-retired", "artv-1", retired): retired,
        }
    )
    both = [
        descriptor_for(tenant, "art-live", "artv-1", live),
        descriptor_for(tenant, "art-retired", "artv-1", retired),
    ]
    index = build()
    rebuild_from_sources(index, ObjectSourceReader(FakeListing(both), objects), tenant_id=tenant)
    assert hits_for(index, "memory entries candidates", "art-retired")

    # The authority deleted that artifact: the next rebuild's set no longer names it.
    report = rebuild_from_sources(index, ObjectSourceReader(FakeListing(both[:1]), objects), tenant_id=tenant)
    assert report.pruned > 0
    assert hits_for(index, "memory entries candidates", "art-retired") == [], (
        "a deleted source must not keep answering retrieval"
    )
    assert hits_for(index, "retention ninety days for logs", "art-live")


def test_a_rebuild_prunes_only_the_kind_it_reads() -> None:
    """A knowledge plane's rows survive an artifact rebuild: one plane cannot delete another's."""
    tenant = TENANT_A
    payload = RUNBOOK.encode()
    objects = FakeObjects({artifact_key(tenant, "art-1", "artv-1", payload): payload})
    store = MemoryStore()
    index = build(store)
    index.index_source(document(MEMORY_TEXT, source_ref="memory-1"))
    rebuild_from_sources(
        index,
        ObjectSourceReader(FakeListing([descriptor_for(tenant, "art-1", "artv-1", payload)]), objects),
        tenant_id=tenant,
    )
    assert store.sources(tenant_id=tenant, source_kind="knowledge_entry") == ["memory-1"]
    assert index.query("memory entries candidates", limit=5)


def test_a_newer_version_supersedes_the_previous_snapshot() -> None:
    tenant = TENANT_A
    first, second = RUNBOOK.encode(), (RUNBOOK + " Retention is thirty days for metrics.").encode()
    objects = FakeObjects(
        {
            artifact_key(tenant, "art-1", "artv-1", first): first,
            artifact_key(tenant, "art-1", "artv-2", second): second,
        }
    )
    index = build()
    report = rebuild_from_sources(
        index,
        ObjectSourceReader(
            FakeListing(
                [
                    descriptor_for(tenant, "art-1", "artv-1", first),
                    descriptor_for(tenant, "art-1", "artv-2", second),
                ]
            ),
            objects,
        ),
        tenant_id=tenant,
    )
    assert report.pruned == 0, "both versions are in the set; supersession removes the older rows"
    hits = index.query("retention thirty days for metrics", limit=5)
    assert {hit.snapshot for hit in hits} == {"artv-2"}, "only the newest snapshot is retrievable"


# ------------------------------------------------------------------- refusals and skips


def test_a_corrupted_object_is_refused_and_writes_nothing() -> None:
    tenant = TENANT_A
    payload = RUNBOOK.encode()
    descriptor = descriptor_for(tenant, "art-1", "artv-1", payload)
    objects = FakeObjects({descriptor.object_key: b"tampered bytes"})
    store = MemoryStore()
    index = build(store)
    with pytest.raises(SourceReadError) as refusal:
        rebuild_from_sources(index, ObjectSourceReader(FakeListing([descriptor]), objects), tenant_id=tenant)
    assert refusal.value.rule_id == RULE_SOURCE_DIGEST
    assert store.count(tenant_id=tenant) == 0, "a refused read leaves the index untouched"


def test_a_missing_object_is_refused() -> None:
    tenant = TENANT_A
    payload = RUNBOOK.encode()
    descriptor = descriptor_for(tenant, "art-1", "artv-1", payload)
    with pytest.raises(AssertionError):
        rebuild_from_sources(
            build(), ObjectSourceReader(FakeListing([descriptor]), FakeObjects({})), tenant_id=tenant
        )


def test_unindexable_content_is_skipped_visibly_not_silently() -> None:
    tenant = TENANT_A
    text = RUNBOOK.encode()
    binary = b"\x00\x01\x02\xff\xfe"
    oversized = b"x" * 4_096
    objects = FakeObjects(
        {
            artifact_key(tenant, "art-text", "artv-1", text): text,
            artifact_key(tenant, "art-binary", "artv-1", binary): binary,
            artifact_key(tenant, "art-huge", "artv-1", oversized): oversized,
        }
    )
    reader = ObjectSourceReader(
        listing=FakeListing(
            [
                descriptor_for(tenant, "art-text", "artv-1", text),
                descriptor_for(tenant, "art-binary", "artv-1", binary),
                descriptor_for(tenant, "art-huge", "artv-1", oversized),
            ]
        ),
        objects=objects,
        max_bytes=1_024,
    )
    index = build()
    report = rebuild_from_sources(index, reader, tenant_id=tenant)

    assert report.chunks > 0
    assert index.query("retention ninety days for logs", limit=5)
    skipped = {skip.source_ref: skip.rule_id for skip in reader.skipped}
    assert skipped == {
        "art-binary": RULE_SOURCE_NOT_TEXT,
        "art-huge": RULE_SOURCE_TOO_LARGE,
    }, "a rebuild reports what it could not index rather than dropping it silently"


def test_a_rebuild_refuses_a_reader_that_mixes_planes() -> None:
    tenant = TENANT_A
    payload = RUNBOOK.encode()
    objects = FakeObjects({artifact_key(tenant, "art-1", "artv-1", payload): payload})
    mixed = SourceDescriptor(
        source_kind="knowledge_entry",
        source_ref="knowledge-1",
        snapshot="rev-1",
        object_key="tenants/t/knowledge/1/rev-1/0000",
        content_digest="",
    )
    with pytest.raises(SourceReadError) as refusal:
        rebuild_from_sources(
            build(),
            ObjectSourceReader(FakeListing([mixed]), objects),
            tenant_id=tenant,
            source_kind=SOURCE_KIND_ARTIFACT,
        )
    assert refusal.value.rule_id == RULE_SOURCE_SHAPE
    assert objects.reads == [], "a foreign plane's bytes are never fetched"


def test_a_rebuild_needs_a_tenant() -> None:
    tenant = TENANT_A
    payload = RUNBOOK.encode()
    objects = FakeObjects({artifact_key(tenant, "art-1", "artv-1", payload): payload})
    with pytest.raises(SourceReadError) as refusal:
        rebuild_from_sources(
            build(),
            ObjectSourceReader(FakeListing([]), objects),
            tenant_id="   ",
        )
    assert refusal.value.rule_id == RULE_SOURCE_TENANT


# ------------------------------------------------------------------ the artifact listing


def test_the_artifact_listing_reads_the_core_007_key_layout() -> None:
    tenant = TENANT_A
    first, second = RUNBOOK.encode(), MEMORY_TEXT.encode()
    keys = [
        artifact_key(tenant, "art-1", "artv-1", first),
        artifact_key(tenant, "art-1", "artv-2", second),
        f"tenants/{tenant}/artifacts/art-1/versions/artv-3/not-a-digest",
        f"tenants/{tenant}/notebooks/nb-1/versions/v1/{'0' * 64}",
        f"tenants/{TENANT_B}/artifacts/art-9/versions/artv-1/{hashlib.sha256(second).hexdigest()}",
    ]
    listing = ArtifactObjectListing(objects=ListingObjects(keys))
    descriptors = listing.list_sources(tenant_id=tenant)
    assert [(d.source_ref, d.snapshot) for d in descriptors] == [
        ("art-1", "artv-1"),
        ("art-1", "artv-2"),
    ], "only this tenant's well-formed artifact version keys are sources"
    assert descriptors[0].content_digest == hashlib.sha256(first).hexdigest()
    assert descriptors[1].content_digest == hashlib.sha256(second).hexdigest()


def test_the_artifact_listing_refuses_without_a_tenant() -> None:
    with pytest.raises(SourceReadError) as refusal:
        ArtifactObjectListing(objects=ListingObjects([])).list_sources(tenant_id="")
    assert refusal.value.rule_id == RULE_SOURCE_TENANT


class ListingObjects:
    """An object reader that only lists, so the listing's parsing is tested on its own."""

    def __init__(self, keys: Sequence[str]) -> None:
        self._keys = tuple(keys)

    def list_keys(self, prefix: str) -> Sequence[str]:
        return tuple(key for key in self._keys if key.startswith(prefix))

    def read(self, object_key: str, *, limit: int) -> bytes:  # pragma: no cover - never called
        raise AssertionError("the listing must not read object bytes")


# --------------------------------------------------------------- the deletion seam


def test_the_deletion_port_removes_a_source_from_retrieval() -> None:
    store = MemoryStore()
    indexes: dict[str, EmbeddingIndex] = {
        TENANT_A: build(store, tenant_id=TENANT_A),
        TENANT_B: build(store, tenant_id=TENANT_B),
    }
    for index in indexes.values():
        index.index_source(document(RUNBOOK, source_ref="knowledge-1"))
        index.index_source(document(MEMORY_TEXT, source_ref="memory-1"))
    port = IndexSourceDeletion(index_for_tenant=lambda tenant_id: indexes[tenant_id])

    removed = port.delete_source(tenant_id=TENANT_A, source_kind="knowledge_entry", source_ref="knowledge-1")
    assert removed > 0
    assert hits_for(indexes[TENANT_A], "retention ninety days for logs", "knowledge-1") == []
    assert hits_for(indexes[TENANT_B], "retention ninety days for logs", "knowledge-1"), (
        "one tenant's deletion cannot touch another tenant's rows"
    )
    assert hits_for(indexes[TENANT_A], "memory entries candidates", "memory-1"), "other sources survive"


def test_the_deletion_port_requires_a_tenant() -> None:
    port = IndexSourceDeletion(index_for_tenant=lambda _tenant: build())
    with pytest.raises(SourceReadError) as refusal:
        port.delete_source(tenant_id="  ", source_kind="knowledge_entry", source_ref="knowledge-1")
    assert refusal.value.rule_id == RULE_SOURCE_TENANT


def test_a_refused_embedding_route_cannot_index_authoritative_sources() -> None:
    """The rebuild path inherits the failover rules: no route means no rows, not partial rows."""
    tenant = TENANT_A
    store = MemoryStore()
    with pytest.raises(EmbeddingError):
        index_for(
            store,
            FailoverEmbedder(providers=(), dimensions=1_536),
            model_id="embedding-route",
            tenant_id=tenant,
        )
    assert store.count(tenant_id=tenant) == 0


def test_source_documents_carry_the_authority_snapshot() -> None:
    tenant = TENANT_A
    payload = RUNBOOK.encode()
    objects = FakeObjects({artifact_key(tenant, "art-1", "artv-9", payload): payload})
    reader = ObjectSourceReader(FakeListing([descriptor_for(tenant, "art-1", "artv-9", payload)]), objects)
    documents = reader.read(tenant_id=tenant)
    assert documents == (
        SourceDocument(
            source_kind=SOURCE_KIND_ARTIFACT,
            source_ref="art-1",
            text=RUNBOOK,
            snapshot="artv-9",
        ),
    )
