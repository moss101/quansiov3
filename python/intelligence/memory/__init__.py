"""Semantic memory: candidate generation and retrieval with provenance. Memory is never recovery
state (INT-007, D-007).

Memory is what the platform chose to remember — a user/workspace decision, or something a verified
run established — kept apart from the transcript, from the Knowledge Fabric and from recovery state.
Nothing here may be read to reconstruct a run or an effect: a restart must succeed with memory
disabled, which is only meaningful if retrieval is an enrichment that can be absent.

Layout:

* `models` — the entry, its scopes, its closed provenance vocabulary and its lifecycle, as pure
  values;
* `store` — the durable store over `public.memory_entries`, one tenant at a time;
* `candidates` — what may be proposed (and by whom), and the scope resolution the owner applies.

Retrieval through the semantic channel (the INT-011 seam) and the deletion path are the remaining
unit of this task.
"""

from __future__ import annotations

from intelligence.memory.candidates import (
    MemoryCandidate,
    MemoryProposalSink,
    ProposalOutcome,
    StoreMemoryProposals,
    new_memory_id,
    propose,
    resolve_scope,
)
from intelligence.memory.models import (
    ID_PREFIX,
    RETRIEVABLE_STATUS,
    TRANSITIONS,
    MemoryEntry,
    MemoryEntryError,
    MemoryProvenance,
    MemoryScope,
    MemoryStatus,
)
from intelligence.memory.store import (
    DEFAULT_LIMIT,
    MAX_LIMIT,
    READ_TABLES,
    STATEMENTS,
    MemoryFabric,
    MemoryStore,
    SqlMemoryStore,
    StatusChange,
    memory_for,
)

__all__ = [
    "DEFAULT_LIMIT",
    "ID_PREFIX",
    "MAX_LIMIT",
    "READ_TABLES",
    "RETRIEVABLE_STATUS",
    "STATEMENTS",
    "TRANSITIONS",
    "MemoryCandidate",
    "MemoryEntry",
    "MemoryEntryError",
    "MemoryFabric",
    "MemoryProposalSink",
    "MemoryProvenance",
    "MemoryScope",
    "MemoryStatus",
    "MemoryStore",
    "ProposalOutcome",
    "SqlMemoryStore",
    "StatusChange",
    "StoreMemoryProposals",
    "memory_for",
    "new_memory_id",
    "propose",
    "resolve_scope",
]
