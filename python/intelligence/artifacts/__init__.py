"""Artifact generation adapters (document, spreadsheet, slides, media, code) — CAP-003.

Canonical owner: `python/intelligence/artifacts`. Proposes versioned source files and
preview/evidence to the Rust artifact authority. Format validation and quality checks
block completion; this plane never commits Artifact/ArtifactVersion rows.
"""

from __future__ import annotations

from intelligence.artifacts.formats import (
    CodeFile,
    CodeSource,
    DataSource,
    DocumentSource,
    PresentationSource,
    Slide,
    SpreadsheetSource,
    parse_code,
    parse_data,
    parse_document,
    parse_kind,
    parse_presentation,
    parse_spreadsheet,
    quality_check,
    serialize_source,
    validate_format,
)
from intelligence.artifacts.models import (
    EDITABLE_KINDS,
    KINDS,
    MEDIA_KINDS,
    SCHEMA_VERSION,
    ArtifactError,
    ArtifactKind,
    ArtifactRecord,
    ArtifactVersion,
    CheckReport,
    Completion,
    Preview,
    digest_of,
)
from intelligence.artifacts.workflow import (
    canonical_body,
    complete,
    create,
    create_from_fixture,
    edit,
    inspect,
    load_skill_manifest,
    preview,
    skill_pack_root,
)

__all__ = [
    "EDITABLE_KINDS",
    "KINDS",
    "MEDIA_KINDS",
    "SCHEMA_VERSION",
    "ArtifactError",
    "ArtifactKind",
    "ArtifactRecord",
    "ArtifactVersion",
    "CheckReport",
    "CodeFile",
    "CodeSource",
    "Completion",
    "DataSource",
    "DocumentSource",
    "PresentationSource",
    "Preview",
    "Slide",
    "SpreadsheetSource",
    "canonical_body",
    "complete",
    "create",
    "create_from_fixture",
    "digest_of",
    "edit",
    "inspect",
    "load_skill_manifest",
    "parse_code",
    "parse_data",
    "parse_document",
    "parse_kind",
    "parse_presentation",
    "parse_spreadsheet",
    "preview",
    "quality_check",
    "serialize_source",
    "skill_pack_root",
    "validate_format",
]
