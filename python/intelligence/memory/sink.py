"""The durable memory sink an intelligence process composes (INT-007, DOMAIN.md §11.4).

The memory owner holds the store driver, not the server package: the process asks this module for
a sink, and gets one only when the operator named the database. Every proposal through it runs
INT-007's candidate gate and lands as a `candidate`.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass

import psycopg

from intelligence.memory.candidates import (
    RULE_SINK,
    MemoryCandidate,
    MemoryProposalSink,
    ProposalOutcome,
    StoreMemoryProposals,
)
from intelligence.memory.models import MemoryEntryError
from intelligence.memory.store import SqlMemoryStore

#: PostgreSQL DSN of the durable memory store (INT-007's `memory_entries`); unset → no sink.
ENV_MEMORY_DATABASE_URL = "QUANSIO_INTELLIGENCE_MEMORY_DATABASE_URL"


def memory_sink(environ: Mapping[str, str] | None = None) -> MemoryProposalSink | None:
    """The gated memory path over the durable store, when the operator configured one.

    Every proposal goes through INT-007's candidate gate (closed provenance, resolved scope, stored
    as a `candidate` that retrieval never returns until promoted). Without a DSN the process has no
    sink and `ProposeMemory` fails closed with `ROUTE_UNAVAILABLE`; nothing is ever acknowledged
    while nothing stores it. Connections are opened per operation, so an unreachable database is a
    refusal of that call rather than a process that cannot start.
    """
    env = os.environ if environ is None else environ
    dsn = env.get(ENV_MEMORY_DATABASE_URL, "").strip()
    if not dsn:
        return None
    return _DriverFailuresRefused(StoreMemoryProposals(store=SqlMemoryStore(lambda: psycopg.connect(dsn))))


@dataclass(frozen=True, slots=True)
class _DriverFailuresRefused:
    """A database failure is a typed refusal of that proposal, never an untyped RPC error.

    The detail names the failure class only: a driver message can carry the DSN.
    """

    inner: MemoryProposalSink

    def propose(self, *, tenant_id: str, workspace_id: str, candidate: MemoryCandidate) -> ProposalOutcome:
        try:
            return self.inner.propose(tenant_id=tenant_id, workspace_id=workspace_id, candidate=candidate)
        except psycopg.Error as failure:
            raise MemoryEntryError(
                "INTERNAL", RULE_SINK, f"the memory store failed ({type(failure).__name__})"
            ) from failure
