"""The authoritative source plane a full rebuild reads (INT-011).

The derived index is rebuildable, and a rebuild is only a proof if it reads the *authoritative*
sources rather than whatever a caller happens to pass. This module is that read, in two halves
that are separate on purpose:

* a **listing** says which sources exist for a tenant. That is an authority claim, so it comes
  from the owner of the source — the runtime's artifact metadata (CORE-007) — through
  [`SourceListing`]. Reading it from object keys is offered as
  [`ArtifactObjectListing`], which is faithful to the CORE-007 key layout but still derives only
  *which* objects exist, never whether an artifact is current, deleted or under legal hold.
* a **reader** fetches the bytes and turns them into a [`SourceDocument`]: the object's sha256 is
  verified against the digest the authority recorded before a single chunk is embedded, so a
  corrupted or substituted object can never enter the index.

Deletion is the same seam from the other side: a source the authoritative set no longer names is
pruned by [`rebuild_from_sources`], and a source deleted on its own plane is removed through the
[`SourceDeletionPort`] that INT-006 and INT-007 call.
"""

from __future__ import annotations

import hashlib
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field, replace
from typing import Protocol

from intelligence.embeddings.index import EmbeddingIndex, RebuildReport, SourceDocument

#: Source kind the artifact plane's rows carry.
SOURCE_KIND_ARTIFACT = "artifact"

#: The CORE-007 object-key layout: `tenants/<tenant>/artifacts/<artifact>/versions/<version>/<sha256>`.
TENANT_PREFIX = "tenants"
ARTIFACT_SEGMENT = "artifacts"
VERSIONS_SEGMENT = "versions"

#: Rules, named so a caller can tell which refusal fired.
RULE_SOURCE_SHAPE = "source.shape"
RULE_SOURCE_DIGEST = "source.digest_mismatch"
RULE_SOURCE_NOT_TEXT = "source.not_text"
RULE_SOURCE_TOO_LARGE = "source.too_large"
RULE_SOURCE_TENANT = "source.tenant_required"

#: Largest source text the reader will index; a bigger object is an attachment, not a source.
MAX_SOURCE_BYTES = 1_000_000


class SourceReadError(RuntimeError):
    """An authoritative source could not be read into a document (never a partial read)."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class SourceDescriptor:
    """One authoritative source: its identity, and where its bytes are.

    `snapshot` is the authority's version identity for the content and `content_digest` is the
    sha256 it recorded; both are carried so the index's provenance key and the digest check use
    the authority's values rather than anything the reader recomputes.
    """

    source_kind: str
    source_ref: str
    snapshot: str
    object_key: str
    content_digest: str = ""


class SourceListing(Protocol):
    """Which sources exist for a tenant, as their owner states it."""

    def list_sources(self, *, tenant_id: str) -> Sequence[SourceDescriptor]: ...


class ObjectBytes(Protocol):
    """Read-only object-storage access to source bytes."""

    def read(self, object_key: str, *, limit: int) -> bytes:
        """At most ``limit + 1`` bytes of the object, so the caller can detect an oversize object."""
        ...


@dataclass(frozen=True, slots=True)
class SourceSkip:
    """A source that exists but cannot become a document, and why."""

    source_ref: str
    object_key: str
    rule_id: str
    detail: str


@dataclass(slots=True)
class ArtifactObjectListing:
    """The CORE-007 artifact object path, as a listing of artifact versions.

    Keys are returned by the store in lexicographic order, which orders versions within an
    artifact ascending, so a rebuild indexes the newest version last and the index's own
    supersession rule deletes the older snapshot's rows. Which version is *current* stays the
    artifact metadata's decision; a runtime that wants to pin it passes the descriptors it
    holds to [`ObjectSourceReader.read_descriptors`] instead of listing.
    """

    objects: ObjectBytes
    prefix_root: str = TENANT_PREFIX

    def list_sources(self, *, tenant_id: str) -> tuple[SourceDescriptor, ...]:
        tenant = _require_tenant(tenant_id)
        prefix = f"{self.prefix_root}/{tenant}/{ARTIFACT_SEGMENT}/"
        descriptors: list[SourceDescriptor] = []
        for key in _list_keys(self.objects, prefix):
            parsed = _parse_artifact_key(key)
            if parsed is not None:
                descriptors.append(parsed)
        return tuple(descriptors)


@dataclass(slots=True)
class ObjectSourceReader:
    """Reads authoritative bytes into indexable documents, verifying every digest.

    Two failure classes are treated differently, and the difference is the point:

    * **corruption is a refusal.** A missing object or bytes that do not match the digest the
      authority recorded abort the read, because the alternative is an index that quietly stands
      in for content nobody can verify.
    * **unindexable content is recorded, not refused.** An artifact that is binary, empty or too
      large to be a text source is skipped — a real tenant holds attachments and media — but
      every skip is kept in [`skipped`][...ObjectSourceReader.skipped] with the rule that fired,
      so a rebuild's coverage is auditable rather than assumed.
    """

    listing: SourceListing
    objects: ObjectBytes
    source_kind: str = SOURCE_KIND_ARTIFACT
    max_bytes: int = MAX_SOURCE_BYTES
    skipped: list[SourceSkip] = field(default_factory=list)

    def read(self, *, tenant_id: str) -> tuple[SourceDocument, ...]:
        """Read this tenant's authoritative sources, in the listing's order."""
        return self.read_descriptors(self.listing.list_sources(tenant_id=_require_tenant(tenant_id)))

    def read_descriptors(self, descriptors: Sequence[SourceDescriptor]) -> tuple[SourceDocument, ...]:
        """Read exactly these sources: the path a runtime uses to scope a rebuild itself."""
        del self.skipped[:]
        documents: list[SourceDocument] = []
        for descriptor in descriptors:
            if descriptor.source_kind != self.source_kind:
                # One reader covers one authoritative plane: mixing them would let a caller's
                # listing decide what a rebuild prunes on the other plane.
                raise SourceReadError(
                    "VALIDATION_SCHEMA",
                    RULE_SOURCE_SHAPE,
                    f"this reader covers {self.source_kind!r}, not {descriptor.source_kind!r}",
                )
            document = self._document_of(descriptor)
            if document is not None:
                documents.append(document)
        return tuple(documents)

    def _document_of(self, descriptor: SourceDescriptor) -> SourceDocument | None:
        raw = self.objects.read(descriptor.object_key, limit=self.max_bytes)
        if len(raw) > self.max_bytes:
            self._skip(
                descriptor,
                RULE_SOURCE_TOO_LARGE,
                f"{len(raw)}+ bytes, over the {self.max_bytes}-byte source bound",
            )
            return None
        if descriptor.content_digest:
            actual = _sha256_hex(raw)
            if actual != descriptor.content_digest:
                # Corruption, not content: the object is not what the authority recorded.
                raise SourceReadError(
                    "VALIDATION_SCHEMA",
                    RULE_SOURCE_DIGEST,
                    f"source {descriptor.source_ref!r} does not match its recorded digest "
                    f"({actual[:12]}… != {descriptor.content_digest[:12]}…)",
                )
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            self._skip(descriptor, RULE_SOURCE_NOT_TEXT, "not UTF-8 text")
            return None
        if not text.strip():
            # An empty artifact carries nothing to retrieve; indexing it would add rows a query
            # could never match.
            self._skip(descriptor, RULE_SOURCE_NOT_TEXT, "empty text")
            return None
        return SourceDocument(
            source_kind=descriptor.source_kind,
            source_ref=descriptor.source_ref,
            text=text,
            snapshot=descriptor.snapshot,
        )

    def _skip(self, descriptor: SourceDescriptor, rule_id: str, detail: str) -> None:
        self.skipped.append(
            SourceSkip(
                source_ref=descriptor.source_ref,
                object_key=descriptor.object_key,
                rule_id=rule_id,
                detail=detail,
            )
        )


def rebuild_from_sources(
    index: EmbeddingIndex,
    reader: ObjectSourceReader,
    *,
    tenant_id: str,
    source_kind: str = SOURCE_KIND_ARTIFACT,
) -> RebuildReport:
    """Rebuild `index` from the authoritative sources, then prune what the set no longer names.

    This is the whole-rebuild operation: every named source is re-indexed from its authoritative
    bytes, and each row of ``source_kind`` whose source the set no longer names is deleted, so a
    source removed upstream stops answering retrieval. Only ``source_kind`` is pruned, because a
    reader covers one plane and deleting another plane's rows would be a cross-owner act.
    """
    if not tenant_id.strip():
        raise SourceReadError(
            "VALIDATION_SCHEMA", RULE_SOURCE_TENANT, "a rebuild must name the tenant it covers"
        )
    documents = reader.read(tenant_id=tenant_id)
    foreign = sorted({document.source_kind for document in documents} - {source_kind})
    if foreign:
        raise SourceReadError(
            "VALIDATION_SCHEMA",
            RULE_SOURCE_SHAPE,
            f"the reader returned source kinds {foreign} for an {source_kind!r} rebuild",
        )
    report = index.rebuild(documents)
    pruned = index.prune(
        source_kind=source_kind, keep_refs=frozenset(document.source_ref for document in documents)
    )
    return replace(report, pruned=pruned)


class SourceDeletionPort(Protocol):
    """The seam a source's owning plane calls when it deletes a source."""

    def delete_source(self, *, tenant_id: str, source_kind: str, source_ref: str) -> int: ...


@dataclass(slots=True)
class IndexSourceDeletion:
    """Deletes a source's rows from semantic retrieval, through a tenant-bound index.

    INT-006 (knowledge) and INT-007 (memory) delete their own rows; each calls this once for the
    deleted source so the derived index cannot keep retrieving text whose source is gone. The
    index is obtained from the composition root's factory, so the tenant stays intrinsic to the
    index and this port never takes one on trust from a caller's data.
    """

    index_for_tenant: Callable[[str], EmbeddingIndex]

    def delete_source(self, *, tenant_id: str, source_kind: str, source_ref: str) -> int:
        if not tenant_id.strip():
            raise SourceReadError(
                "VALIDATION_SCHEMA", RULE_SOURCE_TENANT, "a source deletion must name its tenant"
            )
        return self.index_for_tenant(tenant_id).delete_source(source_kind=source_kind, source_ref=source_ref)


def _require_tenant(tenant_id: str) -> str:
    if not tenant_id.strip():
        raise SourceReadError(
            "VALIDATION_SCHEMA", RULE_SOURCE_TENANT, "a source listing must name its tenant"
        )
    return tenant_id


def _list_keys(objects: object, prefix: str) -> tuple[str, ...]:
    lister = getattr(objects, "list_keys", None)
    if not callable(lister):
        raise SourceReadError(
            "VALIDATION_SCHEMA",
            RULE_SOURCE_SHAPE,
            "the object reader cannot list keys, so no source set can be established",
        )
    return tuple(lister(prefix))


def _parse_artifact_key(key: str) -> SourceDescriptor | None:
    """`tenants/<t>/artifacts/<artifact>/versions/<version>/<sha256>` → a descriptor.

    A key that does not match the layout is not an artifact version (another plane's object under
    the same tenant prefix); it is skipped rather than guessed at.
    """
    parts = key.split("/")
    if len(parts) != 7 or parts[0] != TENANT_PREFIX or parts[2] != ARTIFACT_SEGMENT:
        return None
    if parts[4] != VERSIONS_SEGMENT:
        return None
    tenant, artifact_id, version_id, digest = parts[1], parts[3], parts[5], parts[6]
    if not tenant or not artifact_id or not version_id or len(digest) != 64:
        return None
    return SourceDescriptor(
        source_kind=SOURCE_KIND_ARTIFACT,
        source_ref=artifact_id,
        snapshot=version_id,
        object_key=key,
        content_digest=digest,
    )


def _sha256_hex(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()
