"""Knowledge as durable, provenance-addressable entries (DOMAIN.md §11.4, INT-006).

Knowledge is what the platform has *evaluated* — distinct from the transcript, from recovery state
and from memory. Two structural rules follow from that and are enforced here rather than assumed:

* **An entry is addressed by its provenance.** An entry with no provenance reference is refused,
  because knowledge nobody can trace back to a source is an assertion, and the platform must be able
  to answer "why do we believe this" and "what happens when that source goes away".
* **Derived knowledge follows its source.** Deleting a source *quarantines* the entries derived from
  it (never silently leaves them retrievable, and never deletes them outright: a quarantined entry
  is a recorded decision a person can review, and re-ingesting the source can re-verify it).

The lifecycle is a closed ladder: an unknown state, an illegal edge or a `superseded_by` that does
not match the status is refused when it is read, so no entry can occupy a state the rest of the
platform does not understand. Retrieval is defined by one predicate — only `active` knowledge is
retrievable — which is what makes "removing a source stops it answering retrieval" a property of the
model rather than a convention the index has to remember.
"""

from __future__ import annotations

import math
from collections.abc import Sequence
from dataclasses import dataclass, replace
from enum import StrEnum


class KnowledgeError(ValueError):
    """A refused knowledge artefact, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


#: Refusal rules, named so a caller can tell which one fired.
RULE_ID = "knowledge.id"
RULE_TENANT = "knowledge.tenant"
RULE_SCOPE = "knowledge.scope"
RULE_KIND = "knowledge.kind"
RULE_PROVENANCE = "knowledge.provenance"
RULE_CONFIDENCE = "knowledge.confidence"
RULE_VERSION = "knowledge.version"
RULE_STATUS = "knowledge.status"
RULE_TRANSITION = "knowledge.transition"
RULE_SUPERSEDED = "knowledge.superseded_by"

#: Identity prefix `knowledge_entries.id` constrains with `CHECK (id LIKE 'kn\_%')`.
ID_PREFIX = "kn_"


class KnowledgeScope(StrEnum):
    """How widely an entry applies (DOMAIN.md §11.4)."""

    TENANT = "tenant"
    WORKSPACE = "workspace"
    PACK = "pack"


class KnowledgeStatus(StrEnum):
    """The lifecycle states of an entry; only `ACTIVE` is retrievable."""

    CANDIDATE = "candidate"
    VERIFIED = "verified"
    ACTIVE = "active"
    SUPERSEDED = "superseded"
    QUARANTINED = "quarantined"
    DELETED = "deleted"


#: Legal lifecycle edges. An edge that is not here is refused, not guessed at.
TRANSITIONS: dict[KnowledgeStatus, frozenset[KnowledgeStatus]] = {
    # A candidate is an evaluated but unverified claim; verification may also be withheld.
    KnowledgeStatus.CANDIDATE: frozenset(
        {KnowledgeStatus.VERIFIED, KnowledgeStatus.QUARANTINED, KnowledgeStatus.DELETED}
    ),
    # Verified knowledge is in good standing but not yet in force.
    KnowledgeStatus.VERIFIED: frozenset(
        {
            KnowledgeStatus.ACTIVE,
            KnowledgeStatus.SUPERSEDED,
            KnowledgeStatus.QUARANTINED,
            KnowledgeStatus.DELETED,
        }
    ),
    # Active knowledge is retrievable; it leaves retrieval by being superseded, quarantined or deleted.
    KnowledgeStatus.ACTIVE: frozenset(
        {
            KnowledgeStatus.SUPERSEDED,
            KnowledgeStatus.QUARANTINED,
            KnowledgeStatus.DELETED,
        }
    ),
    KnowledgeStatus.SUPERSEDED: frozenset({KnowledgeStatus.QUARANTINED, KnowledgeStatus.DELETED}),
    # Quarantine is recoverable: re-ingesting the source re-verifies, and a review can re-activate.
    KnowledgeStatus.QUARANTINED: frozenset(
        {KnowledgeStatus.CANDIDATE, KnowledgeStatus.VERIFIED, KnowledgeStatus.DELETED}
    ),
    # Deletion is terminal: a deleted entry is never resurrected.
    KnowledgeStatus.DELETED: frozenset(),
}

#: The one state that retrieval may serve.
RETRIEVABLE_STATUS = KnowledgeStatus.ACTIVE


@dataclass(frozen=True, slots=True)
class Provenance:
    """Where one piece of an entry came from: its source identity and the digest that pins it."""

    source_kind: str
    ref: str
    digest: str = ""
    retrieved_at: str = ""

    def __post_init__(self) -> None:
        if not self.source_kind.strip():
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_PROVENANCE, "provenance source_kind is required")
        if not self.ref.strip():
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_PROVENANCE, "provenance ref is required")

    @property
    def address(self) -> tuple[str, str]:
        """The `(source_kind, ref)` pair an entry can be found by."""
        return (self.source_kind, self.ref)


@dataclass(frozen=True, slots=True)
class KnowledgeEntry:
    """One knowledge entry, validated on construction and never mutated in place."""

    id: str
    tenant_id: str
    scope: KnowledgeScope
    kind: str
    provenance: tuple[Provenance, ...]
    content_ref: str = ""
    workspace_id: str | None = None
    confidence: float = 0.5
    version: int = 1
    status: KnowledgeStatus = KnowledgeStatus.CANDIDATE
    superseded_by: str | None = None
    embedding_ref: str | None = None

    def __post_init__(self) -> None:
        if not self.id.startswith(ID_PREFIX):
            raise KnowledgeError(
                "VALIDATION_SCHEMA", RULE_ID, f"a knowledge id must start with {ID_PREFIX!r}, got {self.id!r}"
            )
        if not self.tenant_id.strip():
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_TENANT, "an entry must name its tenant")
        if not isinstance(self.scope, KnowledgeScope):
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_SCOPE, f"unknown scope {self.scope!r}")
        workspace = (self.workspace_id or "").strip()
        if self.scope is KnowledgeScope.WORKSPACE and not workspace:
            raise KnowledgeError(
                "VALIDATION_SCHEMA", RULE_SCOPE, "a workspace-scoped entry must name its workspace"
            )
        if self.scope is not KnowledgeScope.WORKSPACE and workspace:
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_SCOPE,
                f"a {self.scope.value}-scoped entry must not name a workspace",
            )
        if not self.kind.strip():
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_KIND, "an entry must declare its kind")
        if not self.provenance:
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_PROVENANCE,
                "an entry with no provenance is unaddressable and is refused",
            )
        if not isinstance(self.confidence, (int, float)) or not math.isfinite(self.confidence):
            raise KnowledgeError(
                "VALIDATION_SCHEMA", RULE_CONFIDENCE, f"confidence {self.confidence!r} is not a number"
            )
        if not 0.0 <= float(self.confidence) <= 1.0:
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_CONFIDENCE,
                f"confidence {self.confidence!r} is outside 0..1",
            )
        if self.version < 1:
            raise KnowledgeError(
                "VALIDATION_SCHEMA", RULE_VERSION, f"version {self.version!r} must be at least 1"
            )
        if not isinstance(self.status, KnowledgeStatus):
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_STATUS, f"unknown status {self.status!r}")
        superseded = (self.superseded_by or "").strip()
        if self.status is KnowledgeStatus.SUPERSEDED and not superseded:
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_SUPERSEDED,
                "a superseded entry must name the entry that superseded it",
            )
        if self.status is not KnowledgeStatus.SUPERSEDED and superseded:
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_SUPERSEDED,
                f"superseded_by is only meaningful on a superseded entry, not {self.status.value}",
            )
        if superseded and superseded == self.id:
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_SUPERSEDED, "an entry cannot supersede itself")

    # -- provenance addressing -----------------------------------------------------

    @property
    def provenance_addresses(self) -> tuple[tuple[str, str], ...]:
        """Every `(source_kind, ref)` this entry is addressable by, in order."""
        return tuple(item.address for item in self.provenance)

    def derives_from(self, source_kind: str, ref: str) -> bool:
        """Whether this entry's provenance names that source."""
        return (source_kind, ref) in self.provenance_addresses

    # -- retrieval ------------------------------------------------------------------

    @property
    def retrievable(self) -> bool:
        """Only active knowledge answers retrieval (DOMAIN.md §11.4)."""
        return self.status is RETRIEVABLE_STATUS

    # -- lifecycle ------------------------------------------------------------------

    def with_status(self, status: KnowledgeStatus, *, superseded_by: str | None = None) -> KnowledgeEntry:
        """Apply a lifecycle edge, or refuse it naming both ends.

        A status change is not new content, so `version` is unchanged; supersession creates a
        *new* entry and points this one at it through `superseded_by`.
        """
        if not isinstance(status, KnowledgeStatus):
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_STATUS, f"unknown status {status!r}")
        allowed = TRANSITIONS[self.status]
        if status not in allowed:
            raise KnowledgeError(
                "CONFLICT_STATE",
                RULE_TRANSITION,
                f"{self.status.value} cannot become {status.value}"
                + (f"; legal: {sorted(item.value for item in allowed)}" if allowed else "; it is terminal"),
            )
        successor = superseded_by if status is KnowledgeStatus.SUPERSEDED else None
        return replace(self, status=status, superseded_by=successor)


@dataclass(frozen=True, slots=True)
class QuarantineOutcome:
    """What a source deletion did to the knowledge derived from it."""

    quarantined: tuple[KnowledgeEntry, ...]
    unaffected: tuple[KnowledgeEntry, ...]

    @property
    def changed(self) -> bool:
        return bool(self.quarantined)


def quarantine_derived(entries: Sequence[KnowledgeEntry], *, source_kind: str, ref: str) -> QuarantineOutcome:
    """Quarantine every entry derived from a deleted source (DOMAIN.md §11.4).

    Every entry is accounted for: one derived from the deleted source is quarantined unless it is
    already out of retrieval (superseded or deleted, which a quarantine would only rewrite), and
    every other entry is reported untouched. Nothing is deleted: quarantine is the recorded
    decision, and re-ingesting the source can re-verify what it quarantined.
    """
    if not source_kind.strip() or not ref.strip():
        raise KnowledgeError(
            "VALIDATION_SCHEMA", RULE_PROVENANCE, "a source deletion needs the deleted source's kind and ref"
        )
    quarantined: list[KnowledgeEntry] = []
    unaffected: list[KnowledgeEntry] = []
    for entry in entries:
        if not entry.derives_from(source_kind, ref) or entry.status in (
            KnowledgeStatus.SUPERSEDED,
            KnowledgeStatus.DELETED,
        ):
            unaffected.append(entry)
            continue
        quarantined.append(entry.with_status(KnowledgeStatus.QUARANTINED))
    return QuarantineOutcome(quarantined=tuple(quarantined), unaffected=tuple(unaffected))
