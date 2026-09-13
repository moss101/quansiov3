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
* `store` — the durable store over `public.knowledge_entries`, one tenant at a time.

The derived semantic channel (the pgvector index) is INT-011's, not this package's.
"""

from __future__ import annotations

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
    "RETRIEVABLE_STATUS",
    "STATEMENTS",
    "TRANSITIONS",
    "KnowledgeEntry",
    "KnowledgeError",
    "KnowledgeFabric",
    "KnowledgeScope",
    "KnowledgeStatus",
    "KnowledgeStore",
    "Provenance",
    "QuarantineOutcome",
    "SqlKnowledgeStore",
    "StatusUpdate",
    "knowledge_for",
    "quarantine_derived",
]
