"""What may be proposed as memory, and what the owner does with it (INT-007 unit 3, DOMAIN.md §11.4).

Memory enters the plane from two places the platform actually evaluated: an **explicit user
direction** ("remember that I prefer X") and a **verified run** (work that passed its
CompletionContract). A model or the runtime may *propose* one of those, and that is the whole of its
power here: a [`MemoryCandidate`] carries no identity, no tenant and no lifecycle state — the
canonical owner decides all three — and everything it proposes is stored as a `candidate`, because a
memory is not part of what the platform remembers until something promoted it.

Two gates live here, and they are the owner's rather than the caller's:

* **provenance.** The vocabulary is closed to the two evaluated origins, so nothing enters memory
  because a model found it plausible; an unidentified origin is refused rather than stored.
* **scope.** The runtime passes a *hint* (`MemoryProposalPort.scope`); this module resolves it against
  the memory-scope vocabulary and the workspace the call carries, and refuses a hint that cannot be
  satisfied instead of silently storing memory somewhere else. Policy and capability are already
  checked upstream by the runtime before a candidate reaches this path (RUN-006/RUN-011).

Re-proposing the same claim is not a second memory: the subject, the content and the provenance
together are the claim, and the entry already holding it is returned with `recorded=False`. A
deleted memory is not a duplicate — re-proposing something that was forgotten is a new memory.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol

from intelligence.memory.models import (
    MemoryEntry,
    MemoryEntryError,
    MemoryProvenance,
    MemoryScope,
    MemoryStatus,
    require_instant,
)
from intelligence.memory.store import MAX_LIMIT, MemoryFabric, MemoryStore, memory_for
from intelligence.model_gateway.ids import new_ulid

#: Refusal rules owned by the candidate path.
RULE_CANDIDATE = "memory.candidate"
RULE_SCOPE_HINT = "memory.scope_hint"
RULE_SINK = "memory.sink"


@dataclass(frozen=True, slots=True)
class MemoryCandidate:
    """What may be proposed: something worth remembering, and where it came from.

    There is deliberately no `id`, no `tenant_id` and no `status` field — identity is minted by the
    owner, the tenant is the caller's authority, and everything proposed starts as a `candidate`.
    """

    subject_ref: str
    content: str
    provenance_kind: MemoryProvenance
    provenance_ref: str = ""
    confidence: float = 0.5
    scope_hint: str = ""
    expires_at: str = ""

    def __post_init__(self) -> None:
        if not self.subject_ref.strip():
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_CANDIDATE, "a candidate needs its subject")
        if not self.content.strip():
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_CANDIDATE, "a candidate needs content")
        if not isinstance(self.provenance_kind, MemoryProvenance):
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_CANDIDATE,
                f"{self.provenance_kind!r} is not one of "
                f"{[kind.value for kind in MemoryProvenance]}; memory comes from an explicit user "
                "direction or verified work",
            )
        if self.scope_hint.strip():
            try:
                MemoryScope(self.scope_hint.strip().lower())
            except ValueError as error:
                raise MemoryEntryError(
                    "VALIDATION_SCHEMA",
                    RULE_SCOPE_HINT,
                    f"{self.scope_hint!r} is not one of {[scope.value for scope in MemoryScope]}",
                ) from error
        if self.expires_at.strip():
            require_instant(self.expires_at, field="expires_at")

    @property
    def claim(self) -> tuple[str, str, str, str]:
        """The identity of the claim itself: subject, content and provenance."""
        return (
            self.subject_ref,
            self.content,
            self.provenance_kind.value,
            self.provenance_ref,
        )


@dataclass(frozen=True, slots=True)
class ProposalOutcome:
    """What one candidate did: the stored memory, and whether it recorded or reused it."""

    entry: MemoryEntry
    recorded: bool


class MemoryProposalSink(Protocol):
    """The seam the runtime's `memory.propose` route reaches, one tenant at a time."""

    def propose(
        self, *, tenant_id: str, workspace_id: str, candidate: MemoryCandidate
    ) -> ProposalOutcome: ...


def resolve_scope(candidate: MemoryCandidate, *, workspace_id: str) -> tuple[MemoryScope, str | None]:
    """Resolve the memory scope a candidate is stored under, or refuse the hint.

    A hint wins when it is satisfiable; otherwise the workspace the call carries decides (a
    workspace-scoped memory when there is a workspace, a user-scoped memory when there is not). A
    hint that needs a workspace the call does not carry is refused rather than downgraded, because
    storing a workspace-scoped memory as a user one would widen who can see it.
    """
    workspace = workspace_id.strip()
    hint = candidate.scope_hint.strip().lower()
    if hint:
        scope = MemoryScope(hint)
        if scope is MemoryScope.USER:
            return scope, None
        if not workspace:
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_SCOPE_HINT,
                f"a {scope.value}-scoped memory needs the workspace the call belongs to",
            )
        return scope, workspace
    if workspace:
        return MemoryScope.WORKSPACE, workspace
    return MemoryScope.USER, None


def new_memory_id() -> str:
    """Mint a `mem_` identity for one memory (the schema constrains the prefix)."""
    return f"mem_{new_ulid()}"


def propose(
    fabric: MemoryFabric,
    candidate: MemoryCandidate,
    *,
    workspace_id: str = "",
) -> ProposalOutcome:
    """Store a candidate as a `candidate`, or return the memory it already is."""
    scope, resolved_workspace = resolve_scope(candidate, workspace_id=workspace_id)
    existing = _duplicate_of(fabric, candidate)
    if existing is not None:
        return ProposalOutcome(entry=existing, recorded=False)
    entry = MemoryEntry(
        id=new_memory_id(),
        tenant_id=fabric.tenant_id,
        scope=scope,
        workspace_id=resolved_workspace,
        subject_ref=candidate.subject_ref,
        content=candidate.content,
        provenance_kind=candidate.provenance_kind,
        provenance_ref=candidate.provenance_ref,
        confidence=candidate.confidence,
        status=MemoryStatus.CANDIDATE,
        expires_at=candidate.expires_at,
    )
    return ProposalOutcome(entry=fabric.add(entry), recorded=True)


@dataclass(slots=True)
class StoreMemoryProposals:
    """The production-shaped sink: proposals go through the durable store for the call's tenant."""

    store: MemoryStore

    def propose(self, *, tenant_id: str, workspace_id: str, candidate: MemoryCandidate) -> ProposalOutcome:
        return propose(
            memory_for(self.store, tenant_id=tenant_id),
            candidate,
            workspace_id=workspace_id,
        )


def _duplicate_of(fabric: MemoryFabric, candidate: MemoryCandidate) -> MemoryEntry | None:
    """The memory this candidate already is, among the ones still in force (or None)."""
    for row in fabric.entries(subject_ref=candidate.subject_ref, limit=MAX_LIMIT):
        if row.status is MemoryStatus.DELETED:
            continue
        if (
            row.content,
            row.provenance_kind.value,
            row.provenance_ref,
        ) == (
            candidate.content,
            candidate.provenance_kind.value,
            candidate.provenance_ref,
        ):
            return row
    return None
