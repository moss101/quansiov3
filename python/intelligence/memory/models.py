"""Semantic memory entries and their lifecycle (DOMAIN.md §11.4, INT-007).

Memory is what the platform chose to *remember*: a user/workspace decision, or something a verified
run established. It is deliberately the narrowest of the durable planes, and two rules make it safe
to have at all:

* **Memory is not recovery** (AGENTS.md invariant 7, D-007). Nothing in this module is read to
  reconstruct a run, a checkpoint, protocol state or an effect; memory is an enrichment that can be
  absent, and a restart must succeed with it disabled. The model therefore has no notion of a
  position, a cursor or a pending effect — only content, provenance, scope and a lifecycle.
* **A memory is never self-certified.** `candidate` is where everything a model proposes starts, and
  only `active` memory is retrievable, so nothing becomes part of what the platform remembers without
  an explicit promotion. A memory that expired is not retrievable either, however it got there.

Scopes are `user`, `workspace` and `teammate` (DOMAIN.md §11.4): they say *who* the memory belongs
to, and the store enforces the tenant boundary around all three.
"""

from __future__ import annotations

import math
import re
from dataclasses import dataclass, replace
from enum import StrEnum


class MemoryEntryError(ValueError):
    """A refused memory artefact, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


#: Refusal rules, named so a caller can tell which one fired.
RULE_ID = "memory.id"
RULE_TENANT = "memory.tenant"
RULE_SCOPE = "memory.scope"
RULE_SUBJECT = "memory.subject"
RULE_CONTENT = "memory.content"
RULE_PROVENANCE = "memory.provenance"
RULE_CONFIDENCE = "memory.confidence"
RULE_STATUS = "memory.status"
RULE_TRANSITION = "memory.transition"
RULE_EXPIRY = "memory.expiry"
RULE_INSTANT = "memory.instant"

#: Identity prefix `memory_entries.id` constrains with `CHECK (id LIKE 'mem\_%')`.
ID_PREFIX = "mem_"

#: The one instant shape this plane stores and compares: an ISO-8601 UTC instant, seconds
#: precision, `Z`-suffixed. A canonical form is what makes `is_retrievable_at` a lexicographic
#: comparison and what makes a value written to `TIMESTAMPTZ` read back identically.
INSTANT_PATTERN = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")


def require_instant(value: str, *, field: str) -> str:
    """Validate one instant, refusing anything that is not the canonical form."""
    if not INSTANT_PATTERN.match(value.strip()):
        raise MemoryEntryError(
            "VALIDATION_SCHEMA",
            RULE_INSTANT,
            f"{field} must be an ISO-8601 UTC instant (YYYY-MM-DDTHH:MM:SSZ), got {value!r}",
        )
    return value.strip()


class MemoryScope(StrEnum):
    """Who a memory belongs to (DOMAIN.md §11.4)."""

    USER = "user"
    WORKSPACE = "workspace"
    TEAMMATE = "teammate"


class MemoryProvenance(StrEnum):
    """Where a memory came from: a closed vocabulary, because memory must be attributable."""

    EXPLICIT_USER = "explicit_user"
    VERIFIED_RUN = "verified_run"


class MemoryStatus(StrEnum):
    """The lifecycle states of a memory; only `ACTIVE` is retrievable."""

    CANDIDATE = "candidate"
    ACTIVE = "active"
    DELETED = "deleted"


#: Legal lifecycle edges. An edge that is not here is refused, not guessed at.
TRANSITIONS: dict[MemoryStatus, frozenset[MemoryStatus]] = {
    # A candidate is remembered only once something promoted it.
    MemoryStatus.CANDIDATE: frozenset({MemoryStatus.ACTIVE, MemoryStatus.DELETED}),
    # An active memory leaves by being deleted; promotion is deliberately not reversible here,
    # because "forget this" is the only direction a person asks for.
    MemoryStatus.ACTIVE: frozenset({MemoryStatus.DELETED}),
    # Deletion is terminal: a deleted memory is never resurrected, it is re-proposed.
    MemoryStatus.DELETED: frozenset(),
}

#: The one state retrieval may serve.
RETRIEVABLE_STATUS = MemoryStatus.ACTIVE


@dataclass(frozen=True, slots=True)
class MemoryEntry:
    """One memory entry, validated on construction and never mutated in place."""

    id: str
    tenant_id: str
    scope: MemoryScope
    subject_ref: str
    content: str
    provenance_kind: MemoryProvenance
    provenance_ref: str = ""
    workspace_id: str | None = None
    confidence: float = 0.5
    status: MemoryStatus = MemoryStatus.CANDIDATE
    last_used_at: str = ""
    expires_at: str = ""

    def __post_init__(self) -> None:
        if not self.id.startswith(ID_PREFIX):
            raise MemoryEntryError(
                "VALIDATION_SCHEMA", RULE_ID, f"a memory id must start with {ID_PREFIX!r}, got {self.id!r}"
            )
        if not self.tenant_id.strip():
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_TENANT, "a memory must name its tenant")
        if not isinstance(self.scope, MemoryScope):
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_SCOPE, f"unknown scope {self.scope!r}")
        if not self.subject_ref.strip():
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_SUBJECT,
                "a memory must name its subject (the user, workspace or teammate it is about)",
            )
        if not self.content.strip():
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_CONTENT, "a memory with no content is not one")
        if not isinstance(self.provenance_kind, MemoryProvenance):
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_PROVENANCE,
                f"unknown provenance kind {self.provenance_kind!r}",
            )
        if not isinstance(self.confidence, (int, float)) or not math.isfinite(self.confidence):
            raise MemoryEntryError(
                "VALIDATION_SCHEMA", RULE_CONFIDENCE, f"confidence {self.confidence!r} is not a number"
            )
        if not 0.0 <= float(self.confidence) <= 1.0:
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_CONFIDENCE,
                f"confidence {self.confidence!r} is outside 0..1",
            )
        if not isinstance(self.status, MemoryStatus):
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_STATUS, f"unknown status {self.status!r}")
        if self.expires_at.strip():
            require_instant(self.expires_at, field="expires_at")
        if self.last_used_at.strip():
            require_instant(self.last_used_at, field="last_used_at")
        workspace = (self.workspace_id or "").strip()
        if self.scope is MemoryScope.USER and workspace:
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_SCOPE,
                "a user-scoped memory belongs to the user, so it must not name a workspace",
            )
        if self.scope in (MemoryScope.WORKSPACE, MemoryScope.TEAMMATE) and not workspace:
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_SCOPE,
                f"a {self.scope.value}-scoped memory must name its workspace",
            )

    # -- retrieval ------------------------------------------------------------------

    @property
    def retrievable(self) -> bool:
        """Only active memory answers retrieval (DOMAIN.md §11.4)."""
        return self.status is RETRIEVABLE_STATUS

    @property
    def expired(self) -> bool:
        """Whether the memory has an expiry the caller must compare against its own clock.

        Expiry is data, not a decision taken here: the model carries the instant an operator set and
        an expiry in the past is not retrievable. It never means "delete", because a memory that
        expired may still be reviewed or re-proposed.
        """
        return bool(self.expires_at.strip())

    def is_retrievable_at(self, now: str) -> bool:
        """Whether retrieval may serve this memory at `now` (ISO-8601, compared lexicographically).

        ISO-8601 UTC instants (`YYYY-MM-DDTHH:MM:SSZ`) sort lexicographically, which is what the
        schema's `TIMESTAMPTZ` and the generated contract's string fields both carry, so the
        comparison needs no clock here.
        """
        require_instant(now, field="now")
        if not self.retrievable:
            return False
        expires = self.expires_at.strip()
        return not expires or now <= expires

    # -- lifecycle ------------------------------------------------------------------

    def with_status(self, status: MemoryStatus) -> MemoryEntry:
        """Apply a lifecycle edge, or refuse it naming both ends."""
        if not isinstance(status, MemoryStatus):
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_STATUS, f"unknown status {status!r}")
        allowed = TRANSITIONS[self.status]
        if status not in allowed:
            raise MemoryEntryError(
                "CONFLICT_STATE",
                RULE_TRANSITION,
                f"{self.status.value} cannot become {status.value}"
                + (f"; legal: {sorted(item.value for item in allowed)}" if allowed else "; it is terminal"),
            )
        return replace(self, status=status)

    def used_at(self, now: str) -> MemoryEntry:
        """Record that retrieval used this memory; content and status are untouched."""
        return replace(self, last_used_at=require_instant(now, field="now"))
