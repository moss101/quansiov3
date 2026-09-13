"""The Knowledge Fabric's fail-closed boundaries, without a database (INT-006 unit 2).

These cover what the fabric must refuse *before* it touches the store: an entry of another tenant,
a derived pointer this store does not own, an illegal lifecycle edge, and a write the store says
already happened. The recording double asserts that a refused call reached no I/O at all, which is
the property a database-backed suite cannot show (its refusals are observable but not proof that
nothing was attempted). Every durable property is proved against real PostgreSQL in
`tests/integration/test_knowledge_store.py`.
"""

from __future__ import annotations

from collections.abc import Sequence

import pytest

from intelligence.knowledge.models import (
    KnowledgeEntry,
    KnowledgeError,
    KnowledgeScope,
    KnowledgeStatus,
    Provenance,
)
from intelligence.knowledge.store import (
    MAX_LIMIT,
    RULE_DERIVED_REF,
    RULE_PARAMETERS,
    RULE_STORE,
    RULE_TENANT_SCOPE,
    KnowledgeFabric,
    StatusUpdate,
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
KNOWLEDGE_ID = "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"


class RecordingStore:
    """A store double that records what it was asked to do and answers what the test sets."""

    def __init__(self, *, insert_result: int = 1, rows: Sequence[KnowledgeEntry] = ()) -> None:
        self.insert_result = insert_result
        self.rows = tuple(rows)
        self.entries: list[KnowledgeEntry] = []
        self.updates: list[tuple[StatusUpdate, ...]] = []
        self.reads: list[str] = []

    def insert(self, *, entry: KnowledgeEntry) -> int:
        self.entries.append(entry)
        return self.insert_result

    def get(self, *, tenant_id: str, knowledge_id: str) -> KnowledgeEntry:
        self.reads.append(knowledge_id)
        for row in self.rows:
            if row.id == knowledge_id and row.tenant_id == tenant_id:
                return row
        raise KnowledgeError("NOT_FOUND", "knowledge.not_found", knowledge_id)

    def by_provenance(self, *, tenant_id: str, source_kind: str, ref: str) -> tuple[KnowledgeEntry, ...]:
        self.reads.append(f"{source_kind}:{ref}")
        return tuple(row for row in self.rows if row.derives_from(source_kind, ref))

    def scoped(
        self,
        *,
        tenant_id: str,
        status: KnowledgeStatus | None,
        kind: str | None,
        limit: int,
        offset: int,
    ) -> tuple[KnowledgeEntry, ...]:
        if limit < 1 or limit > MAX_LIMIT:
            raise KnowledgeError("VALIDATION_SCHEMA", RULE_PARAMETERS, f"limit {limit}")
        return tuple(row for row in self.rows if status is None or row.status is status)

    def count(self, *, tenant_id: str, status: KnowledgeStatus | None) -> int:
        return sum(1 for row in self.rows if status is None or row.status is status)

    def update_statuses(self, *, tenant_id: str, updates: Sequence[StatusUpdate]) -> int:
        self.updates.append(tuple(updates))
        return len(updates)


def entry(**overrides: object) -> KnowledgeEntry:
    values: dict[str, object] = {
        "id": KNOWLEDGE_ID,
        "tenant_id": TENANT,
        "scope": KnowledgeScope.TENANT,
        "kind": "runbook",
        "provenance": (Provenance(source_kind="artifact", ref="art-A"),),
        "status": KnowledgeStatus.CANDIDATE,
    }
    values.update(overrides)
    return KnowledgeEntry(**values)  # type: ignore[arg-type]


def fabric(store: RecordingStore | None = None) -> tuple[KnowledgeFabric, RecordingStore]:
    recording = store if store is not None else RecordingStore()
    return KnowledgeFabric(store=recording, tenant_id=TENANT), recording


def test_a_fabric_must_be_bound_to_a_tenant() -> None:
    with pytest.raises(KnowledgeError) as refusal:
        KnowledgeFabric(store=RecordingStore(), tenant_id="   ")
    assert refusal.value.rule_id == "knowledge.tenant"


def test_an_entry_of_another_tenant_is_refused_before_any_write() -> None:
    bound, store = fabric()
    with pytest.raises(KnowledgeError) as refusal:
        bound.add(entry(tenant_id=TENANT_B))
    assert refusal.value.code == "SCOPE_FORBIDDEN"
    assert refusal.value.rule_id == RULE_TENANT_SCOPE
    assert store.entries == [], "a refused add must not reach the store"


def test_the_derived_pointer_is_refused_before_any_write() -> None:
    bound, store = fabric()
    with pytest.raises(KnowledgeError) as refusal:
        bound.add(entry(embedding_ref="emb_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"))
    assert refusal.value.rule_id == RULE_DERIVED_REF
    assert "INT-011" in refusal.value.detail
    assert store.entries == [], "the vector index owns that pointer, so nothing is written"


def test_a_duplicate_identity_is_refused_after_the_store_says_so() -> None:
    bound, store = fabric(RecordingStore(insert_result=0))
    with pytest.raises(KnowledgeError) as refusal:
        bound.add(entry())
    assert refusal.value.code == "CONFLICT_STATE"
    assert refusal.value.rule_id == RULE_STORE
    assert len(store.entries) == 1, "the write was attempted once and not retried"


def test_an_illegal_lifecycle_edge_is_refused_before_any_update() -> None:
    recording = RecordingStore(rows=[entry(status=KnowledgeStatus.CANDIDATE)])
    bound, store = fabric(recording)
    with pytest.raises(KnowledgeError) as refusal:
        bound.set_status(KNOWLEDGE_ID, KnowledgeStatus.ACTIVE)
    assert refusal.value.rule_id == "knowledge.transition"
    assert store.updates == [], "the model decides the edge, so no update is attempted"


def test_a_legal_edge_carries_the_state_the_caller_read() -> None:
    recording = RecordingStore(rows=[entry(status=KnowledgeStatus.VERIFIED)])
    bound, store = fabric(recording)
    bound.set_status(KNOWLEDGE_ID, KnowledgeStatus.ACTIVE)
    assert len(store.updates) == 1
    update = store.updates[0][0]
    assert update.expected_status is KnowledgeStatus.VERIFIED
    assert update.new_status is KnowledgeStatus.ACTIVE
    assert update.superseded_by is None


def test_a_deletion_with_nothing_derived_writes_nothing() -> None:
    recording = RecordingStore(rows=[entry(provenance=(Provenance(source_kind="artifact", ref="art-B"),))])
    bound, store = fabric(recording)
    outcome = bound.quarantine_source("artifact", "art-A")
    assert outcome.quarantined == ()
    assert not outcome.changed
    assert store.updates == [], "nothing derived matched, so there is nothing to write"


def test_the_read_bounds_are_checked_by_the_store_contract() -> None:
    bound, _store = fabric()
    with pytest.raises(KnowledgeError) as refusal:
        bound.entries(limit=MAX_LIMIT + 1)
    assert refusal.value.rule_id == RULE_PARAMETERS


def test_the_fabric_reads_only_active_knowledge_for_retrieval() -> None:
    recording = RecordingStore(
        rows=[
            entry(id=KNOWLEDGE_ID, status=KnowledgeStatus.ACTIVE),
            entry(id="kn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB", status=KnowledgeStatus.VERIFIED),
        ]
    )
    bound, _store = fabric(recording)
    assert [row.id for row in bound.retrievable()] == [KNOWLEDGE_ID]
