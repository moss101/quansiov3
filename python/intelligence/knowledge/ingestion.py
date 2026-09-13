"""Ingestion into the Knowledge Fabric and the forgetting path (INT-006 unit 3, DOMAIN.md §11.4).

Knowledge enters the fabric from two places the platform has actually evaluated: an **approved
source** (an artifact a person approved for reuse) and a **verified run outcome** (work that passed
its CompletionContract). A model may *propose* either, and that is the whole of its power here: a
[`KnowledgeProposal`] carries no identity, no tenant and no lifecycle state — those are the
canonical owner's to decide — and everything it proposes is stored as a `candidate`, because a
model cannot certify its own knowledge into retrieval (`AGENTS.md` invariant 10).

The forgetting path is the other half. A source's removal has two consequences in two planes, and
they are applied in the order of their authority:

1. the knowledge **derived from** the source is quarantined in the fabric — the authoritative
   decision, which also retains the rows so the decision is auditable and reversible;
2. the source's **own rows** leave the derived vector index through INT-011's
   [`SourceDeletionPort`][intelligence.embeddings.sources.SourceDeletionPort], which is what bounds
   retrieval visibility (DOSSIER.md §21.3: ≤ 60 s) rather than leaving a stale hit behind.

The first is committed before the second is attempted, so a derived-index failure can never lose the
authoritative decision: the index is rebuildable, a quarantine that was never recorded is not.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass

from intelligence.embeddings.sources import SourceDeletionPort
from intelligence.knowledge.models import (
    KnowledgeEntry,
    KnowledgeError,
    KnowledgeScope,
    KnowledgeStatus,
    Provenance,
    QuarantineOutcome,
)
from intelligence.knowledge.store import KnowledgeFabric
from intelligence.model_gateway.ids import new_ulid

#: The provenance kinds knowledge may be derived from: a closed vocabulary, not free text.
PROVENANCE_APPROVED_SOURCE = "approved_source"
PROVENANCE_VERIFIED_RUN = "verified_run"
PROVENANCE_KINDS: tuple[str, ...] = (PROVENANCE_APPROVED_SOURCE, PROVENANCE_VERIFIED_RUN)

#: The source kind knowledge contributes to the derived index (INT-011 keys rows by source kind
#: and ref), so a withdrawn entry is addressable in exactly the plane that indexed it.
SOURCE_KIND_KNOWLEDGE = "knowledge_entry"

#: Refusal rules owned by ingestion.
RULE_PROPOSAL = "knowledge.proposal"
RULE_PROVENANCE_KIND = "knowledge.provenance_kind"
RULE_INGESTION = "knowledge.ingestion"


@dataclass(frozen=True, slots=True)
class KnowledgeProposal:
    """What may be proposed: content worth remembering and the source it came from.

    There is deliberately no `id`, no `tenant_id`, no `scope` and no `status` field. Identity is
    minted by the owner, the tenant and scope are the caller's authority, and the lifecycle starts
    at `candidate` for everything a model proposes.
    """

    kind: str
    source_kind: str
    ref: str
    provenance_kind: str = PROVENANCE_APPROVED_SOURCE
    content_ref: str = ""
    digest: str = ""
    retrieved_at: str = ""
    confidence: float = 0.5

    def __post_init__(self) -> None:
        if self.provenance_kind not in PROVENANCE_KINDS:
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_PROVENANCE_KIND,
                f"{self.provenance_kind!r} is not one of {list(PROVENANCE_KINDS)}; knowledge is "
                "derived from an approved source or a verified run outcome",
            )
        if not self.kind.strip():
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_PROPOSAL, "a proposal must name its kind")

    @property
    def provenance(self) -> Provenance:
        """The provenance reference the owner will store for this proposal."""
        return Provenance(
            source_kind=self.source_kind,
            ref=self.ref,
            digest=self.digest,
            retrieved_at=self.retrieved_at,
        )


@dataclass(frozen=True, slots=True)
class IngestionResult:
    """What one proposal did: the stored entry, and whether it created or reused it."""

    entry: KnowledgeEntry
    created: bool


@dataclass(frozen=True, slots=True)
class SourceDeletionOutcome:
    """What forgetting a source did in both planes."""

    quarantined: tuple[KnowledgeEntry, ...]
    unaffected: tuple[KnowledgeEntry, ...]
    #: Rows of the removed source itself that left the derived index.
    index_rows_removed: int
    #: Rows of the knowledge this deletion quarantined that left the derived index.
    quarantined_rows_removed: int = 0

    @property
    def changed(self) -> bool:
        return bool(self.quarantined) or self.index_rows_removed > 0 or self.quarantined_rows_removed > 0


def new_knowledge_id() -> str:
    """Mint a `kn_` identity for one knowledge entry (the schema constrains the prefix)."""
    return f"kn_{new_ulid()}"


def ingest(
    fabric: KnowledgeFabric,
    proposal: KnowledgeProposal,
    *,
    scope: KnowledgeScope = KnowledgeScope.TENANT,
    workspace_id: str | None = None,
    knowledge_id: str | None = None,
) -> IngestionResult:
    """Store a proposal as a `candidate`, or return the entry it duplicates.

    Re-ingesting the same claim is not an error and must not create a second entry: the same kind,
    content address and provenance among the entries still in force is the same knowledge, so the
    existing entry is returned with `created=False`. An entry that has been superseded or withdrawn
    is not a duplicate — a claim that was retired and is proposed again is new knowledge.
    """
    existing = _duplicate_of(fabric, proposal)
    if existing is not None:
        return IngestionResult(entry=existing, created=False)
    entry = KnowledgeEntry(
        id=knowledge_id if knowledge_id is not None else new_knowledge_id(),
        tenant_id=fabric.tenant_id,
        scope=scope,
        workspace_id=workspace_id,
        kind=proposal.kind,
        content_ref=proposal.content_ref,
        provenance=(proposal.provenance,),
        confidence=proposal.confidence,
        status=KnowledgeStatus.CANDIDATE,
    )
    return IngestionResult(entry=fabric.add(entry), created=True)


def ingest_many(
    fabric: KnowledgeFabric,
    proposals: Sequence[KnowledgeProposal],
    *,
    scope: KnowledgeScope = KnowledgeScope.TENANT,
    workspace_id: str | None = None,
) -> tuple[IngestionResult, ...]:
    """Ingest a batch in order; each proposal's outcome is reported rather than summarised."""
    return tuple(ingest(fabric, proposal, scope=scope, workspace_id=workspace_id) for proposal in proposals)


def forget_source(
    fabric: KnowledgeFabric,
    deletion: SourceDeletionPort,
    *,
    source_kind: str,
    source_ref: str,
) -> SourceDeletionOutcome:
    """Forget a removed source: quarantine what derived from it, then clear both index sets.

    Three steps, in the order of their authority:

    1. the knowledge derived from the source is quarantined in the fabric — the authoritative
       decision, which retains the rows so it stays auditable and reversible;
    2. the source's own rows leave the derived index;
    3. the rows of every entry this deletion quarantined leave it as well, because a quarantined
       entry must stop answering retrieval (DOSSIER.md §21.3) rather than stay reachable through
       the semantic channel.

    The quarantine is committed before either index call, and the calls are not wrapped: an
    unreachable index is reported to the caller rather than hidden, and the authoritative decision
    is already durable. The index is rebuildable; a decision that was never recorded is not.
    """
    quarantine: QuarantineOutcome = fabric.quarantine_source(source_kind, source_ref)
    removed = deletion.delete_source(
        tenant_id=fabric.tenant_id, source_kind=source_kind, source_ref=source_ref
    )
    quarantined_rows = 0
    for entry in quarantine.quarantined:
        quarantined_rows += deletion.delete_source(
            tenant_id=fabric.tenant_id,
            source_kind=SOURCE_KIND_KNOWLEDGE,
            source_ref=entry.id,
        )
    return SourceDeletionOutcome(
        quarantined=quarantine.quarantined,
        unaffected=quarantine.unaffected,
        index_rows_removed=removed,
        quarantined_rows_removed=quarantined_rows,
    )


def forget_entry(
    fabric: KnowledgeFabric,
    deletion: SourceDeletionPort,
    knowledge_id: str,
) -> tuple[KnowledgeEntry, SourceDeletionOutcome]:
    """Withdraw a knowledge entry: its lifecycle edge, then the forgetting of it as a source.

    A withdrawn entry is addressable in two ways and both are honoured: what *it* superseded is
    quarantined as knowledge derived from it, and its own rows leave the derived index, so nothing
    answers retrieval for it. An entry already withdrawn is not re-transitioned (deletion is
    terminal); the forgetting still runs, because a previous attempt may not have reached the index.
    """
    entry = fabric.get(knowledge_id)
    if entry.status is not KnowledgeStatus.DELETED:
        entry = fabric.delete(knowledge_id)
    outcome = forget_source(fabric, deletion, source_kind=SOURCE_KIND_KNOWLEDGE, source_ref=knowledge_id)
    return entry, outcome


def _duplicate_of(fabric: KnowledgeFabric, proposal: KnowledgeProposal) -> KnowledgeEntry | None:
    """The entry this proposal already is, among those still in force (or None)."""
    address = proposal.provenance.address
    for candidate in fabric.by_provenance(*address):
        if candidate.kind != proposal.kind:
            continue
        if candidate.content_ref != proposal.content_ref:
            continue
        if candidate.status in (KnowledgeStatus.SUPERSEDED, KnowledgeStatus.DELETED):
            continue
        return candidate
    return None
