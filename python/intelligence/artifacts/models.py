"""Versioned artifact sources (DOMAIN.md §10.1, CAP-003).

Intelligence proposes document, spreadsheet, slide, media and code artifacts; the Rust artifact
authority (CORE-007) commits metadata and bytes. Two rules are enforced here rather than assumed:

* **A source is versioned, not a chat blob.** Each version is a structured file with a kind, a
  media type, a content digest and a parent pointer. Bytes are immutable per version: an edit
  mints a new identity.
* **Completion is gated.** Format validation and task-specific quality checks run before a
  completion is proposed. A failed check does not return a Completion.
"""

from __future__ import annotations

import hashlib
from collections.abc import Mapping
from dataclasses import dataclass
from enum import StrEnum

from intelligence.model_gateway.ids import new_ulid

RULE_KIND = "artifact.kind"
RULE_SHAPE = "artifact.shape"
RULE_FORMAT = "artifact.format"
RULE_QUALITY = "artifact.quality"
RULE_OPAQUE = "artifact.opaque_blob"
RULE_VERSION = "artifact.version"
RULE_MEDIA = "artifact.media_type"

KINDS: tuple[str, ...] = (
    "document",
    "spreadsheet",
    "presentation",
    "code",
    "data",
    "image",
    "audio",
    "video",
    "archive",
    "other",
)

EDITABLE_KINDS: tuple[str, ...] = ("document", "spreadsheet", "presentation", "code", "data")
MEDIA_KINDS: tuple[str, ...] = ("image", "audio", "video")

DEFAULT_MEDIA_TYPES: Mapping[str, str] = {
    "document": "text/markdown",
    "spreadsheet": "text/csv",
    "presentation": "application/vnd.quansio.slides+json",
    "code": "application/vnd.quansio.code+json",
    "data": "application/json",
    "image": "image/png",
    "audio": "audio/mpeg",
    "video": "video/mp4",
    "archive": "application/zip",
    "other": "application/octet-stream",
}

MEDIA_KIND_PREFIX: Mapping[str, str] = {
    "image": "image/",
    "audio": "audio/",
    "video": "video/",
}

SCHEMA_VERSION = "v1"
ID_PREFIX = "art_"
VERSION_PREFIX = "artv_"


class ArtifactError(ValueError):
    """A refused artifact proposal, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


class ArtifactKind(StrEnum):
    """DOMAIN.md §10.1 artifact kinds."""

    DOCUMENT = "document"
    SPREADSHEET = "spreadsheet"
    PRESENTATION = "presentation"
    CODE = "code"
    DATA = "data"
    IMAGE = "image"
    AUDIO = "audio"
    VIDEO = "video"
    ARCHIVE = "archive"
    OTHER = "other"

    @classmethod
    def parse(cls, value: object) -> ArtifactKind:
        if not isinstance(value, str) or value not in KINDS:
            raise ArtifactError("VALIDATION_SCHEMA", RULE_KIND, f"{value!r} is not an artifact kind")
        return cls(value)

    @property
    def editable(self) -> bool:
        return self.value in EDITABLE_KINDS

    @property
    def is_media(self) -> bool:
        return self.value in MEDIA_KINDS


def new_artifact_id(*, seed: str | None = None) -> str:
    return f"{ID_PREFIX}{new_ulid(seed=seed)}"


def new_version_id(*, seed: str | None = None) -> str:
    return f"{VERSION_PREFIX}{new_ulid(seed=seed)}"


def digest_of(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


@dataclass(frozen=True, slots=True)
class ArtifactVersion:
    """One immutable version of an artifact source (DOMAIN.md §10.1 ArtifactVersion)."""

    id: str
    artifact_id: str
    seq: int
    content_digest: str
    size_bytes: int
    media_type: str
    body: bytes
    parent_version_id: str | None = None
    produced_by_run_id: str | None = None
    produced_by_step_id: str | None = None
    produced_by_user_id: str | None = None

    def __post_init__(self) -> None:
        if not self.id.startswith(VERSION_PREFIX):
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_VERSION, f"a version id must start with {VERSION_PREFIX!r}"
            )
        if not self.artifact_id.startswith(ID_PREFIX):
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_SHAPE, f"an artifact id must start with {ID_PREFIX!r}"
            )
        if self.seq < 1:
            raise ArtifactError("VALIDATION_BOUNDS", RULE_VERSION, "version seq must be >= 1")
        if len(self.content_digest) != 64:
            raise ArtifactError("VALIDATION_SCHEMA", RULE_SHAPE, "content_digest must be sha256")
        if digest_of(self.body) != self.content_digest:
            raise ArtifactError(
                "VALIDATION_SCHEMA",
                RULE_SHAPE,
                "content_digest does not match the version bytes",
            )
        if self.size_bytes != len(self.body):
            raise ArtifactError("VALIDATION_BOUNDS", RULE_SHAPE, "size_bytes must equal body length")
        if not self.media_type.strip():
            raise ArtifactError("VALIDATION_SCHEMA", RULE_MEDIA, "a version needs a media type")

    def text(self) -> str:
        try:
            return self.body.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "this version is not UTF-8 text") from error


@dataclass(frozen=True, slots=True)
class ArtifactRecord:
    """An artifact identity plus its ordered, immutable versions."""

    id: str
    workspace_id: str
    kind: ArtifactKind
    title: str
    versions: tuple[ArtifactVersion, ...]
    role: str | None = None
    origin_kind: str = "run"
    origin_ref: str = ""

    def __post_init__(self) -> None:
        if not self.id.startswith(ID_PREFIX):
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_SHAPE, f"an artifact id must start with {ID_PREFIX!r}"
            )
        if not self.workspace_id.strip():
            raise ArtifactError("VALIDATION_SCHEMA", RULE_SHAPE, "an artifact needs a workspace")
        if not self.title.strip():
            raise ArtifactError("VALIDATION_SCHEMA", RULE_SHAPE, "an artifact needs a title")
        if not self.versions:
            raise ArtifactError("VALIDATION_SCHEMA", RULE_VERSION, "an artifact needs at least one version")
        seqs = [version.seq for version in self.versions]
        if seqs != list(range(1, len(seqs) + 1)):
            raise ArtifactError("VALIDATION_SCHEMA", RULE_VERSION, "versions must be contiguous seq 1..n")
        for version in self.versions:
            if version.artifact_id != self.id:
                raise ArtifactError("VALIDATION_SCHEMA", RULE_VERSION, "a version must name its artifact")

    @property
    def current(self) -> ArtifactVersion:
        return self.versions[-1]

    @property
    def current_version_id(self) -> str:
        return self.current.id

    def version(self, version_id: str) -> ArtifactVersion:
        for item in self.versions:
            if item.id == version_id:
                return item
        raise ArtifactError("NOT_FOUND", RULE_VERSION, f"version {version_id} is not on this artifact")


@dataclass(frozen=True, slots=True)
class Preview:
    """A non-executing preview plus the evidence that produced it (APP-008 / CAP-003)."""

    artifact_id: str
    version_id: str
    media_type: str
    content_digest: str
    excerpt: str
    evidence_kind: str = "digest"


@dataclass(frozen=True, slots=True)
class Completion:
    """A proposed completion bound to a validated version. Never returned when validation fails."""

    artifact_id: str
    version_id: str
    content_digest: str
    seq: int
    kind: str
    title: str


@dataclass(frozen=True, slots=True)
class CheckReport:
    """One format or quality check outcome."""

    ok: bool
    rule_id: str
    detail: str
