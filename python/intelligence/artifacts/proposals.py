"""Governed artifact tool calls (CAP-003, DOMAIN.md §7.5, §10.1).

A run makes or edits an artifact by proposing ``artifact.create`` or ``artifact.update`` — tools
the Rust runtime validates against its registry, authorizes as a ``content.upload`` effect and
executes on the server, where the artifact authority checks the source again and stores it as an
immutable version. This module builds those calls from a source: the canonical bytes of a
structured source, their SHA-256 ``content_digest`` (the effect's idempotency input and the
host's integrity check), and the kind, title and role.

It never proposes a source that fails its check: :func:`check_source` runs first and an invalid
source raises :class:`ArtifactError` here, before any call exists. The authority would refuse it
anyway; failing in the plane gives the model the reason one step earlier.
"""

from __future__ import annotations

import base64
from collections.abc import Mapping
from dataclasses import dataclass

from intelligence.artifacts.formats import check_source, parse_kind, serialize_source
from intelligence.artifacts.models import (
    DEFAULT_MEDIA_TYPES,
    ArtifactError,
    ArtifactKind,
    CheckReport,
    digest_of,
)

CREATE_TOOL = "artifact.create"
UPDATE_TOOL = "artifact.update"


@dataclass(frozen=True, slots=True)
class ArtifactToolCall:
    """One governed tool call: the tool name and the arguments its registry schema accepts."""

    tool: str
    arguments: Mapping[str, object]
    check: CheckReport

    @property
    def content_digest(self) -> str:
        return str(self.arguments["content_digest"])


def _kind(kind: ArtifactKind | str) -> ArtifactKind:
    return kind if isinstance(kind, ArtifactKind) else ArtifactKind.parse(kind)


def _source_bytes(kind: ArtifactKind, source: object) -> bytes:
    """Canonical bytes: a structured source is serialized, an editable kind's text is
    re-canonicalized through its parser, media bytes pass through unchanged."""
    if isinstance(source, str):
        source = source.encode("utf-8")
    if isinstance(source, (bytes, bytearray)):
        payload = bytes(source)
        if kind.editable:
            try:
                return serialize_source(parse_kind(kind, payload))
            except ArtifactError:
                # Leave it as given: the check below reports why it is not a source.
                return payload
        return payload
    return serialize_source(source)


def _content_arguments(kind: ArtifactKind, payload: bytes) -> dict[str, object]:
    arguments: dict[str, object] = {"content_digest": digest_of(payload)}
    if kind.editable:
        arguments["content"] = payload.decode("utf-8")
    else:
        arguments["content_base64"] = base64.b64encode(payload).decode("ascii")
    return arguments


def _checked(kind: ArtifactKind, payload: bytes, media_type: str) -> CheckReport:
    report = check_source(kind, payload, media_type)
    if not report.ok:
        raise ArtifactError("VALIDATION_SCHEMA", report.rule_id, report.detail)
    return report


def create_call(
    *,
    kind: ArtifactKind | str,
    title: str,
    role: str,
    source: object,
    media_type: str | None = None,
) -> ArtifactToolCall:
    """The ``artifact.create`` call for a new artifact whose first version is ``source``."""
    parsed = _kind(kind)
    if not title.strip() or len(title) > 200:
        raise ArtifactError("VALIDATION_BOUNDS", "artifact.title", "title must be 1..=200 characters")
    if not role.strip():
        raise ArtifactError("VALIDATION_SCHEMA", "artifact.role", "a role names what the artifact is for")
    declared = media_type or DEFAULT_MEDIA_TYPES[parsed.value]
    payload = _source_bytes(parsed, source)
    report = _checked(parsed, payload, declared)
    arguments: dict[str, object] = {
        "role": role,
        "kind": parsed.value,
        "title": title.strip(),
        "media_type": declared,
        **_content_arguments(parsed, payload),
    }
    return ArtifactToolCall(tool=CREATE_TOOL, arguments=arguments, check=report)


def update_call(
    *,
    artifact_id: str,
    kind: ArtifactKind | str,
    source: object,
    media_type: str | None = None,
) -> ArtifactToolCall:
    """The ``artifact.update`` call that adds ``source`` as the artifact's next version."""
    parsed = _kind(kind)
    if not artifact_id.startswith("art_"):
        raise ArtifactError("VALIDATION_SCHEMA", "artifact.id", f"{artifact_id!r} is not an artifact id")
    declared = media_type or DEFAULT_MEDIA_TYPES[parsed.value]
    payload = _source_bytes(parsed, source)
    report = _checked(parsed, payload, declared)
    arguments: dict[str, object] = {
        "artifact_id": artifact_id,
        "media_type": declared,
        **_content_arguments(parsed, payload),
    }
    return ArtifactToolCall(tool=UPDATE_TOOL, arguments=arguments, check=report)
