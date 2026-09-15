"""Format validation and canonical round-trip for artifact kinds (CAP-003).

Each editable kind has a closed source shape. A payload that is a chat transcript, an unknown
key, or a malformed file is refused — completion never sees it. Canonical serialize/parse is
byte-stable for the same structured value so an edit can be round-tripped through versioned
source files rather than opaque blobs.
"""

from __future__ import annotations

import csv
import io
import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from intelligence.artifacts.models import (
    DEFAULT_MEDIA_TYPES,
    MEDIA_KIND_PREFIX,
    RULE_FORMAT,
    RULE_KIND,
    RULE_MEDIA,
    RULE_OPAQUE,
    RULE_QUALITY,
    SCHEMA_VERSION,
    ArtifactError,
    ArtifactKind,
    CheckReport,
)

DOCUMENT_SCRIPT_MARKERS: tuple[str, ...] = (
    "<script",
    "</script",
    "javascript:",
    "onerror=",
    "onload=",
)

SLIDE_KEYS: frozenset[str] = frozenset({"title", "body", "notes"})
SLIDE_DOC_KEYS: frozenset[str] = frozenset({"schema_version", "slides"})
CODE_FILE_KEYS: frozenset[str] = frozenset({"path", "content"})
CODE_DOC_KEYS: frozenset[str] = frozenset({"schema_version", "language", "files"})
DATA_DOC_KEYS: frozenset[str] = frozenset({"schema_version", "rows", "columns"})


@dataclass(frozen=True, slots=True)
class DocumentSource:
    title: str
    body: str

    def canonical_bytes(self) -> bytes:
        body = self.body.strip("\n")
        text = f"# {self.title}\n\n{body}\n" if body else f"# {self.title}\n"
        return text.encode("utf-8")


@dataclass(frozen=True, slots=True)
class SpreadsheetSource:
    headers: tuple[str, ...]
    rows: tuple[tuple[str, ...], ...]

    def canonical_bytes(self) -> bytes:
        buffer = io.StringIO()
        writer = csv.writer(buffer, lineterminator="\n")
        writer.writerow(self.headers)
        writer.writerows(self.rows)
        return buffer.getvalue().encode("utf-8")


@dataclass(frozen=True, slots=True)
class Slide:
    title: str
    body: str = ""
    notes: str = ""

    def as_mapping(self) -> dict[str, str]:
        payload = {"title": self.title, "body": self.body}
        if self.notes:
            payload["notes"] = self.notes
        return payload


@dataclass(frozen=True, slots=True)
class PresentationSource:
    slides: tuple[Slide, ...]

    def canonical_bytes(self) -> bytes:
        payload = {
            "schema_version": SCHEMA_VERSION,
            "slides": [slide.as_mapping() for slide in self.slides],
        }
        return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")


@dataclass(frozen=True, slots=True)
class CodeFile:
    path: str
    content: str

    def as_mapping(self) -> dict[str, str]:
        return {"path": self.path, "content": self.content}


@dataclass(frozen=True, slots=True)
class CodeSource:
    language: str
    files: tuple[CodeFile, ...]

    def canonical_bytes(self) -> bytes:
        payload = {
            "schema_version": SCHEMA_VERSION,
            "language": self.language,
            "files": [item.as_mapping() for item in self.files],
        }
        return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")


@dataclass(frozen=True, slots=True)
class DataSource:
    columns: tuple[str, ...]
    rows: tuple[Mapping[str, Any], ...]

    def canonical_bytes(self) -> bytes:
        payload = {
            "schema_version": SCHEMA_VERSION,
            "columns": list(self.columns),
            "rows": [dict(row) for row in self.rows],
        }
        return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")


def refuse_opaque_chat_blob(payload: bytes) -> None:
    """Refuse a chat transcript disguised as an artifact source."""
    try:
        decoded = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return
    if _looks_like_message(decoded):
        raise ArtifactError(
            "VALIDATION_SCHEMA",
            RULE_OPAQUE,
            "artifact source is an opaque chat blob; use a versioned document, spreadsheet, "
            "presentation or code file",
        )
    if isinstance(decoded, list) and decoded and all(_looks_like_message(item) for item in decoded):
        raise ArtifactError(
            "VALIDATION_SCHEMA",
            RULE_OPAQUE,
            "artifact source is a chat transcript; artifacts are versioned source files",
        )


def _looks_like_message(value: object) -> bool:
    if not isinstance(value, Mapping):
        return False
    keys = set(value)
    if "schema_version" in keys:
        return False
    return "role" in keys and "content" in keys


def validate_media_type(kind: ArtifactKind, media_type: str) -> None:
    if not media_type.strip():
        raise ArtifactError("VALIDATION_SCHEMA", RULE_MEDIA, "media type is required")
    prefix = MEDIA_KIND_PREFIX.get(kind.value)
    if prefix is not None and not media_type.startswith(prefix):
        raise ArtifactError(
            "VALIDATION_SCHEMA",
            RULE_MEDIA,
            f"{kind.value} artifacts require a {prefix}* media type, got {media_type!r}",
        )
    if kind is ArtifactKind.DOCUMENT and media_type not in {"text/markdown", "text/plain"}:
        raise ArtifactError(
            "VALIDATION_SCHEMA", RULE_MEDIA, f"document media type {media_type!r} is not markdown or text"
        )
    if kind is ArtifactKind.SPREADSHEET and media_type not in {"text/csv", "text/tab-separated-values"}:
        raise ArtifactError(
            "VALIDATION_SCHEMA", RULE_MEDIA, f"spreadsheet media type {media_type!r} is not csv or tsv"
        )


def parse_document(payload: bytes) -> DocumentSource:
    refuse_opaque_chat_blob(payload)
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "document is not UTF-8") from error
    lowered = text.lower()
    for marker in DOCUMENT_SCRIPT_MARKERS:
        if marker in lowered:
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_FORMAT, f"document contains active content marker {marker!r}"
            )
    lines = text.splitlines()
    heading = next((line for line in lines if line.strip()), "")
    if not heading.startswith("# "):
        raise ArtifactError(
            "VALIDATION_SCHEMA", RULE_FORMAT, "document must start with a markdown '# Title' heading"
        )
    title = heading[2:].strip()
    if not title:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "document title is empty")
    body_lines = text.splitlines()
    heading_index = next(i for i, line in enumerate(body_lines) if line.strip())
    body = "\n".join(body_lines[heading_index + 1 :]).strip("\n")
    return DocumentSource(title=title, body=body)


def parse_spreadsheet(payload: bytes) -> SpreadsheetSource:
    refuse_opaque_chat_blob(payload)
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "spreadsheet is not UTF-8") from error
    rows = list(csv.reader(io.StringIO(text)))
    if not rows:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "spreadsheet has no header row")
    headers = tuple(cell.strip() for cell in rows[0])
    if not headers or any(not header for header in headers):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "spreadsheet headers must be non-empty")
    if len(set(headers)) != len(headers):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "spreadsheet headers must be unique")
    width = len(headers)
    data: list[tuple[str, ...]] = []
    for index, row in enumerate(rows[1:], start=2):
        if len(row) != width:
            raise ArtifactError(
                "VALIDATION_SCHEMA",
                RULE_FORMAT,
                f"spreadsheet row {index} has {len(row)} columns, expected {width}",
            )
        data.append(tuple(row))
    if not data:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "spreadsheet needs at least one data row")
    return SpreadsheetSource(headers=headers, rows=tuple(data))


def parse_presentation(payload: bytes) -> PresentationSource:
    refuse_opaque_chat_blob(payload)
    raw = _json_object(payload, "presentation")
    unknown = sorted(set(raw) - SLIDE_DOC_KEYS)
    if unknown:
        raise ArtifactError(
            "VALIDATION_SCHEMA", RULE_FORMAT, f"presentation has unknown keys: {', '.join(unknown)}"
        )
    if raw.get("schema_version") != SCHEMA_VERSION:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "presentation schema_version must be v1")
    slides_raw = raw.get("slides")
    if not isinstance(slides_raw, Sequence) or isinstance(slides_raw, (str, bytes)):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "presentation slides must be a list")
    slides: list[Slide] = []
    for index, entry in enumerate(slides_raw):
        if not isinstance(entry, Mapping):
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"slide {index} is not an object")
        extra = sorted(set(entry) - SLIDE_KEYS)
        if extra:
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_FORMAT, f"slide {index} has unknown keys: {', '.join(extra)}"
            )
        title = entry.get("title")
        if not isinstance(title, str) or not title.strip():
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"slide {index} needs a title")
        body = entry.get("body", "")
        notes = entry.get("notes", "")
        if not isinstance(body, str) or not isinstance(notes, str):
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"slide {index} body/notes must be text")
        slides.append(Slide(title=title.strip(), body=body, notes=notes))
    if not slides:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "presentation needs at least one slide")
    return PresentationSource(slides=tuple(slides))


def parse_code(payload: bytes) -> CodeSource:
    refuse_opaque_chat_blob(payload)
    raw = _json_object(payload, "code")
    unknown = sorted(set(raw) - CODE_DOC_KEYS)
    if unknown:
        raise ArtifactError(
            "VALIDATION_SCHEMA", RULE_FORMAT, f"code artifact has unknown keys: {', '.join(unknown)}"
        )
    if raw.get("schema_version") != SCHEMA_VERSION:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "code schema_version must be v1")
    language = raw.get("language")
    if not isinstance(language, str) or not language.strip():
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "code artifact needs a language")
    files_raw = raw.get("files")
    if not isinstance(files_raw, Sequence) or isinstance(files_raw, (str, bytes)):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "code files must be a list")
    files: list[CodeFile] = []
    seen: set[str] = set()
    for index, entry in enumerate(files_raw):
        if not isinstance(entry, Mapping):
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"code file {index} is not an object")
        extra = sorted(set(entry) - CODE_FILE_KEYS)
        if extra:
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_FORMAT, f"code file {index} has unknown keys: {', '.join(extra)}"
            )
        path = entry.get("path")
        content = entry.get("content")
        if not isinstance(path, str) or not path.strip():
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"code file {index} needs a path")
        if not isinstance(content, str):
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"code file {index} content must be text")
        if path in seen:
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"duplicate code path {path!r}")
        seen.add(path)
        files.append(CodeFile(path=path, content=content))
    if not files:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "code artifact needs at least one file")
    return CodeSource(language=language.strip(), files=tuple(files))


def parse_data(payload: bytes) -> DataSource:
    refuse_opaque_chat_blob(payload)
    raw = _json_object(payload, "data")
    unknown = sorted(set(raw) - DATA_DOC_KEYS)
    if unknown:
        raise ArtifactError(
            "VALIDATION_SCHEMA", RULE_FORMAT, f"data artifact has unknown keys: {', '.join(unknown)}"
        )
    if raw.get("schema_version") != SCHEMA_VERSION:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "data schema_version must be v1")
    columns_raw = raw.get("columns")
    rows_raw = raw.get("rows")
    if not isinstance(columns_raw, Sequence) or isinstance(columns_raw, (str, bytes)):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "data columns must be a list")
    columns = tuple(str(item) for item in columns_raw)
    if not columns or any(not column.strip() for column in columns):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "data columns must be non-empty names")
    if not isinstance(rows_raw, Sequence) or isinstance(rows_raw, (str, bytes)):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "data rows must be a list")
    rows: list[Mapping[str, Any]] = []
    for index, entry in enumerate(rows_raw):
        if not isinstance(entry, Mapping):
            raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"data row {index} is not an object")
        extra = sorted(set(entry) - set(columns))
        if extra:
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_FORMAT, f"data row {index} has unknown columns: {', '.join(extra)}"
            )
        rows.append(dict(entry))
    if not rows:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "data artifact needs at least one row")
    return DataSource(columns=columns, rows=tuple(rows))


def parse_kind(kind: ArtifactKind, payload: bytes) -> object:
    match kind:
        case ArtifactKind.DOCUMENT:
            return parse_document(payload)
        case ArtifactKind.SPREADSHEET:
            return parse_spreadsheet(payload)
        case ArtifactKind.PRESENTATION:
            return parse_presentation(payload)
        case ArtifactKind.CODE:
            return parse_code(payload)
        case ArtifactKind.DATA:
            return parse_data(payload)
        case _:
            refuse_opaque_chat_blob(payload)
            if not payload:
                raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"{kind.value} payload is empty")
            return payload


def serialize_source(source: object) -> bytes:
    if isinstance(source, (DocumentSource, SpreadsheetSource, PresentationSource, CodeSource, DataSource)):
        return source.canonical_bytes()
    if isinstance(source, (bytes, bytearray)):
        return bytes(source)
    raise ArtifactError("VALIDATION_SCHEMA", RULE_KIND, f"cannot serialize {type(source).__name__}")


def validate_format(kind: ArtifactKind, payload: bytes, media_type: str | None = None) -> CheckReport:
    """Run format validation. Returns a report; does not raise, so completion can block on it."""
    declared = media_type or DEFAULT_MEDIA_TYPES[kind.value]
    try:
        validate_media_type(kind, declared)
        parsed = parse_kind(kind, payload)
        if kind.editable:
            round_trip = serialize_source(parsed)
            again = parse_kind(kind, round_trip)
            if serialize_source(again) != round_trip:
                raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, "canonical round-trip did not converge")
    except ArtifactError as error:
        return CheckReport(ok=False, rule_id=error.rule_id, detail=error.detail)
    return CheckReport(ok=True, rule_id=RULE_FORMAT, detail=f"{kind.value} format is valid")


def quality_check(kind: ArtifactKind, payload: bytes) -> CheckReport:
    """Task-specific quality gates that run after format validation."""
    try:
        parsed = parse_kind(kind, payload)
    except ArtifactError as error:
        return CheckReport(ok=False, rule_id=error.rule_id, detail=error.detail)
    if isinstance(parsed, DocumentSource) and not parsed.body.strip():
        return CheckReport(ok=False, rule_id=RULE_QUALITY, detail="document body is empty")
    if isinstance(parsed, SpreadsheetSource) and not parsed.rows:
        return CheckReport(ok=False, rule_id=RULE_QUALITY, detail="spreadsheet has no data rows")
    if isinstance(parsed, PresentationSource) and any(not slide.body.strip() for slide in parsed.slides):
        return CheckReport(ok=False, rule_id=RULE_QUALITY, detail="every slide needs a body")
    if isinstance(parsed, CodeSource) and any(not item.content.strip() for item in parsed.files):
        return CheckReport(ok=False, rule_id=RULE_QUALITY, detail="every code file needs content")
    if isinstance(parsed, DataSource) and not parsed.rows:
        return CheckReport(ok=False, rule_id=RULE_QUALITY, detail="data artifact has no rows")
    if kind.is_media and len(payload) < 8:
        return CheckReport(ok=False, rule_id=RULE_QUALITY, detail="media payload is too small to be real")
    return CheckReport(ok=True, rule_id=RULE_QUALITY, detail=f"{kind.value} quality holds")


def _json_object(payload: bytes, label: str) -> dict[str, Any]:
    try:
        decoded = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"{label} is not JSON") from error
    if not isinstance(decoded, dict):
        raise ArtifactError("VALIDATION_SCHEMA", RULE_FORMAT, f"{label} must be a JSON object")
    return decoded
