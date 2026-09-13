"""Knowledge Fabric: authoritative durable semantic knowledge with provenance, versioning and
lifecycle (INT-006).

The fabric is the platform's durable *evaluated* knowledge, kept apart from the transcript, from
memory and from recovery state. An entry carries provenance, a scope and a lifecycle, and retrieval
may serve only `active` knowledge. Deleting a source *quarantines* the knowledge derived from it
rather than deleting it, so provenance and audit survive and re-ingesting the source can re-verify
what it quarantined.

Layout:

* `models` — the entry, its provenance address, the closed lifecycle ladder and the quarantine
  rules, as pure values;
* `store` — the durable store over `public.knowledge_entries`, one tenant at a time;
* `ingestion` — what a model may propose, and the forgetting path a removed source takes;
* `indexing` — the semantic channel: what belongs in INT-011's derived index, and retrieval that
  agrees with the fabric.

The derived index itself is INT-011's, not this package's: `indexing` decides what belongs in the
channel and lets INT-011 own how it is stored and searched.
"""

from __future__ import annotations

from intelligence.knowledge.indexing import (
    DescribedKnowledgeText,
    IndexSyncReport,
    KnowledgeIndexer,
    KnowledgeText,
    RetrievedKnowledge,
    retrieve,
)
from intelligence.knowledge.ingestion import (
    PROVENANCE_APPROVED_SOURCE,
    PROVENANCE_KINDS,
    PROVENANCE_VERIFIED_RUN,
    SOURCE_KIND_KNOWLEDGE,
    IngestionResult,
    KnowledgeProposal,
    SourceDeletionOutcome,
    forget_entry,
    forget_source,
    ingest,
    ingest_many,
    new_knowledge_id,
)
from intelligence.knowledge.models import (
    ID_PREFIX,
    RETRIEVABLE_STATUS,
    TRANSITIONS,
    KnowledgeEntry,
    KnowledgeError,
    KnowledgeScope,
    KnowledgeStatus,
    Provenance,
    QuarantineOutcome,
    quarantine_derived,
)
from intelligence.knowledge.store import (
    DEFAULT_LIMIT,
    MAX_LIMIT,
    STATEMENTS,
    KnowledgeFabric,
    KnowledgeStore,
    SqlKnowledgeStore,
    StatusUpdate,
    knowledge_for,
)

__all__ = [
    "DEFAULT_LIMIT",
    "ID_PREFIX",
    "MAX_LIMIT",
    "PROVENANCE_APPROVED_SOURCE",
    "PROVENANCE_KINDS",
    "PROVENANCE_VERIFIED_RUN",
    "RETRIEVABLE_STATUS",
    "SOURCE_KIND_KNOWLEDGE",
    "STATEMENTS",
    "TRANSITIONS",
    "DescribedKnowledgeText",
    "IndexSyncReport",
    "IngestionResult",
    "KnowledgeEntry",
    "KnowledgeError",
    "KnowledgeFabric",
    "KnowledgeIndexer",
    "KnowledgeProposal",
    "KnowledgeScope",
    "KnowledgeStatus",
    "KnowledgeStore",
    "KnowledgeText",
    "Provenance",
    "QuarantineOutcome",
    "RetrievedKnowledge",
    "SourceDeletionOutcome",
    "SqlKnowledgeStore",
    "StatusUpdate",
    "forget_entry",
    "forget_source",
    "ingest",
    "ingest_many",
    "knowledge_for",
    "new_knowledge_id",
    "quarantine_derived",
    "retrieve",
]
