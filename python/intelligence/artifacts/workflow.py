"""Artifact-edit workflow: create, versioned edit, preview, gated completion (CAP-003).

The workflow is a skill/tool procedure, not a second store. It proposes `artifact.create` /
`artifact.update` payloads with versioned source files and preview evidence. Completion is
refused when format validation or a quality check fails — the previous version stays current.
"""

from __future__ import annotations

from pathlib import Path

import yaml

from intelligence.artifacts.formats import (
    quality_check,
    serialize_source,
    validate_format,
    validate_media_type,
)
from intelligence.artifacts.models import (
    DEFAULT_MEDIA_TYPES,
    RULE_QUALITY,
    ArtifactError,
    ArtifactKind,
    ArtifactRecord,
    ArtifactVersion,
    CheckReport,
    Completion,
    Preview,
    digest_of,
    new_artifact_id,
    new_version_id,
)
from intelligence.skills.models import SkillManifest

PREVIEW_EXCERPT_CHARS = 240
SKILL_PACK_REL = Path("packs") / "skills" / "artifacts"


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[3]


def skill_pack_root() -> Path:
    return _repo_root() / SKILL_PACK_REL


def fixture_path(name: str) -> Path:
    return skill_pack_root() / "fixtures" / name


def load_skill_manifest(path: Path | None = None) -> SkillManifest:
    """Load the artifact-edit skill pack; unknown keys are refused by SkillManifest."""
    manifest_path = path or (skill_pack_root() / "skill.yaml")
    raw = yaml.safe_load(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ArtifactError("VALIDATION_SCHEMA", "artifact.skill", "skill.yaml must be an object")
    return SkillManifest.parse(raw)


def _version(
    *,
    artifact_id: str,
    seq: int,
    media_type: str,
    body: bytes,
    parent_version_id: str | None,
    seed: str | None,
    produced_by_run_id: str | None = None,
) -> ArtifactVersion:
    return ArtifactVersion(
        id=new_version_id(seed=None if seed is None else f"{seed}:v{seq}"),
        artifact_id=artifact_id,
        seq=seq,
        content_digest=digest_of(body),
        size_bytes=len(body),
        media_type=media_type,
        body=body,
        parent_version_id=parent_version_id,
        produced_by_run_id=produced_by_run_id,
    )


def create(
    *,
    workspace_id: str,
    kind: ArtifactKind | str,
    title: str,
    body: bytes | str,
    media_type: str | None = None,
    role: str | None = None,
    origin_kind: str = "run",
    origin_ref: str = "",
    produced_by_run_id: str | None = None,
    seed: str | None = None,
) -> ArtifactRecord:
    """Mint an artifact with version seq=1 from structured source bytes."""
    parsed_kind = kind if isinstance(kind, ArtifactKind) else ArtifactKind.parse(kind)
    payload = body.encode("utf-8") if isinstance(body, str) else body
    declared = media_type or DEFAULT_MEDIA_TYPES[parsed_kind.value]
    validate_media_type(parsed_kind, declared)
    artifact_id = new_artifact_id(seed=seed)
    version = _version(
        artifact_id=artifact_id,
        seq=1,
        media_type=declared,
        body=payload,
        parent_version_id=None,
        seed=seed,
        produced_by_run_id=produced_by_run_id,
    )
    return ArtifactRecord(
        id=artifact_id,
        workspace_id=workspace_id,
        kind=parsed_kind,
        title=title,
        versions=(version,),
        role=role,
        origin_kind=origin_kind,
        origin_ref=origin_ref,
    )


def create_from_fixture(
    name: str,
    *,
    workspace_id: str,
    kind: ArtifactKind | str,
    title: str,
    seed: str | None = None,
) -> ArtifactRecord:
    path = fixture_path(name)
    return create(
        workspace_id=workspace_id,
        kind=kind,
        title=title,
        body=path.read_bytes(),
        seed=seed or name,
        origin_kind="run",
        origin_ref=f"fixture:{name}",
    )


def edit(
    record: ArtifactRecord,
    body: bytes | str,
    *,
    media_type: str | None = None,
    seed: str | None = None,
) -> ArtifactRecord:
    """Append an immutable version. The previous version remains addressable."""
    payload = body.encode("utf-8") if isinstance(body, str) else body
    current = record.current
    declared = media_type or current.media_type
    validate_media_type(record.kind, declared)
    nxt = _version(
        artifact_id=record.id,
        seq=current.seq + 1,
        media_type=declared,
        body=payload,
        parent_version_id=current.id,
        seed=seed or f"{record.id}:edit",
        produced_by_run_id=current.produced_by_run_id,
    )
    return ArtifactRecord(
        id=record.id,
        workspace_id=record.workspace_id,
        kind=record.kind,
        title=record.title,
        versions=(*record.versions, nxt),
        role=record.role,
        origin_kind=record.origin_kind,
        origin_ref=record.origin_ref,
    )


def preview(record: ArtifactRecord, *, version_id: str | None = None) -> Preview:
    """Non-executing preview: digest + bounded excerpt. Never runs HTML/JS/SVG."""
    version = record.version(version_id) if version_id else record.current
    try:
        excerpt = version.text()[:PREVIEW_EXCERPT_CHARS]
    except ArtifactError:
        excerpt = f"sha256:{version.content_digest}"
    return Preview(
        artifact_id=record.id,
        version_id=version.id,
        media_type=version.media_type,
        content_digest=version.content_digest,
        excerpt=excerpt,
        evidence_kind="digest",
    )


def inspect(record: ArtifactRecord) -> tuple[CheckReport, CheckReport]:
    """Format then quality. Format failure is enough to block completion."""
    version = record.current
    fmt = validate_format(record.kind, version.body, version.media_type)
    if not fmt.ok:
        return fmt, CheckReport(ok=False, rule_id=RULE_QUALITY, detail="quality not run: format failed")
    return fmt, quality_check(record.kind, version.body)


def complete(record: ArtifactRecord) -> Completion:
    """Propose completion only when format and quality hold.

    A failed format check raises and does not return a Completion, so a run cannot close on
    a malformed artifact.
    """
    fmt, quality = inspect(record)
    if not fmt.ok:
        raise ArtifactError(
            "VALIDATION_SCHEMA",
            fmt.rule_id,
            f"format validation blocked completion: {fmt.detail}",
        )
    if not quality.ok:
        raise ArtifactError(
            "VALIDATION_BOUNDS",
            quality.rule_id,
            f"quality check blocked completion: {quality.detail}",
        )
    current = record.current
    return Completion(
        artifact_id=record.id,
        version_id=current.id,
        content_digest=current.content_digest,
        seq=current.seq,
        kind=record.kind.value,
        title=record.title,
    )


def canonical_body(kind: ArtifactKind | str, body: bytes | str) -> bytes:
    """Parse then serialize so callers store the canonical source file."""
    parsed_kind = kind if isinstance(kind, ArtifactKind) else ArtifactKind.parse(kind)
    payload = body.encode("utf-8") if isinstance(body, str) else body
    from intelligence.artifacts.formats import parse_kind

    return serialize_source(parse_kind(parsed_kind, payload))
