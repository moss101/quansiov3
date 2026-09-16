"""Capability Compiler ingestion pipeline (CAP-004).

Turns authoritative enterprise material — documents, SOPs, policies, API
descriptions, permission matrices and verified historical work — into a
*candidate* Business Capability Pack draft. Three rules are structural:

* **Candidate, never executable.** The compiler's output is data: a
  `PackDraft` whose elements carry their own provenance and whose status is
  `candidate`. Qualification/promotion is CAP-005's; nothing here can run.
* **No credentials and no cross-tenant material.** Ingestion scans for
  credential-shaped content and refuses it; every source must name the same
  tenant as the compiler run, so a private cross-tenant document cannot leak
  into a pack (ingestion provenance / credential leak / tenant isolation).
* **Decomposition is traceable.** Every requirement in the draft names the
  source document and the offset span it came from, so a reviewer can walk
  output back to input.
"""

from __future__ import annotations

import hashlib
import re
from collections.abc import Sequence
from dataclasses import dataclass, field

#: Refusal rules, named so a caller can tell which one fired.
RULE_TENANT = "compiler.tenant"
RULE_CREDENTIAL = "compiler.credential"
RULE_SOURCE = "compiler.source"
RULE_EMPTY = "compiler.empty"

#: Credential-shaped content the pack must never carry (case-insensitive).
CREDENTIAL_PATTERNS: tuple[re.Pattern[str], ...] = tuple(
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        r"\bsk-[A-Za-z0-9_-]{16,}",
        r"\bghp_[A-Za-z0-9]{20,}",
        r"\bAKIA[0-9A-Z]{16}\b",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
        r"\bpassword\s*[:=]\s*\S+",
        r"\bapi[_-]?key\s*[:=]\s*\S+",
        r"\bbearer\s+[A-Za-z0-9._-]{16,}",
    )
)


class CompilerError(ValueError):
    """A refused compilation, naming the rule that refused it."""

    def __init__(self, code: str, detail: str) -> None:
        super().__init__(f"{code}: {detail}")
        self.code = code
        self.detail = detail


@dataclass(frozen=True, slots=True)
class IngestionSource:
    """One authoritative input document, already tenant-scoped by the caller."""

    source_id: str
    tenant_id: str
    kind: str
    content: str

    def __post_init__(self) -> None:
        if not self.source_id.strip() or not self.kind.strip():
            raise CompilerError(RULE_SOURCE, "a source needs an id and a kind")
        if not self.tenant_id.startswith("tn_"):
            raise CompilerError(RULE_TENANT, f"source {self.source_id!r} must name its tenant")


def scan_for_credentials(content: str) -> list[str]:
    """The credential patterns found in `content`, empty when clean."""
    return [pattern.pattern for pattern in CREDENTIAL_PATTERNS if pattern.search(content)]


@dataclass(frozen=True, slots=True)
class Requirement:
    """One decomposed requirement with provenance back to its source span."""

    kind: str
    text: str
    source_id: str
    span: tuple[int, int]
    provenance_digest: str

    def as_mapping(self) -> dict[str, object]:
        return {
            "kind": self.kind,
            "text": self.text,
            "source_id": self.source_id,
            "span": list(self.span),
            "provenance_digest": self.provenance_digest,
        }


@dataclass(slots=True)
class PackDraft:
    """A candidate pack: data with provenance, never executable."""

    name: str
    tenant_id: str
    status: str = "candidate"
    requirements: list[Requirement] = field(default_factory=list)
    sources: list[str] = field(default_factory=list)

    def content_digest(self) -> str:
        """Content address over the requirements, so qualification pins what it reviewed."""
        material = "\n".join(
            f"{item.kind}|{item.source_id}|{item.span[0]}-{item.span[1]}|{item.text}"
            for item in self.requirements
        )
        return hashlib.sha256(material.encode("utf-8")).hexdigest()

    def as_mapping(self) -> dict[str, object]:
        return {
            "name": self.name,
            "tenant_id": self.tenant_id,
            "status": self.status,
            "content_digest": self.content_digest(),
            "requirements": [item.as_mapping() for item in self.requirements],
            "sources": list(self.sources),
        }


_SENTINELS = (
    ("policy", re.compile(r"must\s+not|shall\s+not|is prohibited|is required", re.IGNORECASE)),
    ("process", re.compile(r"step\s+\d|first|then|after that|procedure", re.IGNORECASE)),
    ("knowledge", re.compile(r"\bis defined as|\bmeans\b|refers to", re.IGNORECASE)),
    ("tool", re.compile(r"\bapi\b|\bendpoint\b|\btool\b|\bdashboard\b", re.IGNORECASE)),
    ("evaluation", re.compile(r"verify|acceptance|check that|confirm that", re.IGNORECASE)),
)


def _classify(sentence: str) -> str | None:
    for kind, pattern in _SENTINELS:
        if pattern.search(sentence):
            return kind
    return None


def _digest(source_id: str, span: tuple[int, int], sentence: str) -> str:
    material = f"{source_id}|{span[0]}-{span[1]}|{sentence}"
    return hashlib.sha256(material.encode("utf-8")).hexdigest()


def compile_pack(
    name: str,
    tenant_id: str,
    sources: Sequence[IngestionSource],
) -> PackDraft:
    """Ingest and decompose sources into a candidate `PackDraft`.

    # Errors
    [`CompilerError`] when a source is empty, carries credential-shaped content,
    names a different tenant than the run, or no source survives decomposition.
    """
    if not sources:
        raise CompilerError(RULE_EMPTY, "a pack needs at least one source document")
    draft = PackDraft(name=name, tenant_id=tenant_id)
    for source in sources:
        if source.tenant_id != tenant_id:
            raise CompilerError(
                RULE_TENANT,
                f"source {source.source_id!r} belongs to {source.tenant_id!r}, "
                f"not the compilation tenant {tenant_id!r}; private cross-tenant data is excluded",
            )
        found = scan_for_credentials(source.content)
        if found:
            raise CompilerError(
                RULE_CREDENTIAL,
                f"source {source.source_id!r} carries credential-shaped content ({', '.join(found)}); "
                "raw credentials are excluded from generated packs",
            )
        draft.sources.append(source.source_id)
        cursor = 0
        for sentence in re.split(r"(?<=[.!?])\s+|\n+", source.content):
            sentence = sentence.strip()
            if not sentence:
                cursor += 1
                continue
            start = source.content.find(sentence, cursor)
            span = (start, start + len(sentence))
            cursor = span[1]
            kind = _classify(sentence)
            if kind is None or len(sentence.split()) < 3:
                continue
            draft.requirements.append(
                Requirement(
                    kind=kind,
                    text=sentence,
                    source_id=source.source_id,
                    span=span,
                    provenance_digest=_digest(source.source_id, span, sentence),
                )
            )
    if not draft.requirements:
        raise CompilerError(RULE_EMPTY, "decomposition produced no requirements from the sources")
    return draft
