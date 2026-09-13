"""Semantic memory: candidate generation and retrieval with provenance. Memory is never recovery
state (INT-007, D-007).

Memory is what the platform chose to remember — a user/workspace decision, or something a verified
run established — kept apart from the transcript, from the Knowledge Fabric and from recovery state.
Nothing here may be read to reconstruct a run or an effect: a restart must succeed with memory
disabled, which is only meaningful if retrieval is an enrichment that can be absent.

Layout:

* `models` — the entry, its scopes, its closed provenance vocabulary and its lifecycle, as pure
  values.

The durable store over `public.memory_entries`, candidate creation, retrieval and the deletion path
(the INT-011 seam) are the remaining units of this task.
"""

from __future__ import annotations

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

__all__ = [
    "ID_PREFIX",
    "RETRIEVABLE_STATUS",
    "TRANSITIONS",
    "MemoryEntry",
    "MemoryEntryError",
    "MemoryProvenance",
    "MemoryScope",
    "MemoryStatus",
]
