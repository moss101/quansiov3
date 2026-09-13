"""The memory fabric's fail-closed boundaries, without a database (INT-007 unit 2).

These cover what the fabric must refuse *before* it touches the store — a memory of another tenant, an
illegal lifecycle edge, a duplicate identity, out-of-bounds reads — and what retrieval must exclude:
a candidate, a deleted memory, and one whose expiry has passed. The recording double asserts that a
refused call reached no I/O at all, which a database-backed suite cannot show. Every durable property
is proved against real PostgreSQL in `tests/integration/test_memory_store.py`.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import replace

import pytest

from intelligence.memory.models import (
    MemoryEntry,
    MemoryEntryError,
    MemoryProvenance,
    MemoryScope,
    MemoryStatus,
)
from intelligence.memory.store import (
    MAX_LIMIT,
    RULE_PARAMETERS,
    RULE_STORE,
    RULE_TENANT_SCOPE,
    MemoryFabric,
    StatusChange,
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
SUBJECT = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
MEMORY_ID = "mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
NOW = "2026-09-13T12:00:00Z"


class RecordingStore:
    """A store double that stores, records and applies the same guards the real one does."""

    def __init__(self, *, insert_result: int = 1, rows: Sequence[MemoryEntry] = ()) -> None:
        self.insert_result = insert_result
        self.rows = tuple(rows)
        self.entries: list[MemoryEntry] = []
        self.status_changes: list[StatusChange] = []
        self.marks: list[tuple[str, str]] = []

    def insert(self, *, entry: MemoryEntry) -> int:
        self.entries.append(entry)
        if self.insert_result == 1:
            self.rows = (*self.rows, entry)
        return self.insert_result

    def get(self, *, tenant_id: str, memory_id: str) -> MemoryEntry:
        for row in self.rows:
            if row.id == memory_id and row.tenant_id == tenant_id:
                return row
        raise MemoryEntryError("NOT_FOUND", "memory.not_found", memory_id)

    def scoped(
        self,
        *,
        tenant_id: str,
        status: MemoryStatus | None,
        scope: MemoryScope | None,
        subject_ref: str | None,
        retrievable_only: bool,
        now: str,
        limit: int,
        offset: int,
    ) -> tuple[MemoryEntry, ...]:
        if limit < 1 or limit > MAX_LIMIT or offset < 0:
            raise MemoryEntryError("VALIDATION_SCHEMA", RULE_PARAMETERS, f"limit {limit} offset {offset}")
        selected = [
            row
            for row in self.rows
            if (status is None or row.status is status)
            and (scope is None or row.scope is scope)
            and (not subject_ref or row.subject_ref == subject_ref)
            and (not retrievable_only or row.is_retrievable_at(now))
        ]
        return tuple(sorted(selected, key=lambda row: row.id)[offset : offset + limit])

    def count(
        self,
        *,
        tenant_id: str,
        status: MemoryStatus | None,
        scope: MemoryScope | None,
        subject_ref: str | None,
    ) -> int:
        return sum(
            1
            for row in self.rows
            if (status is None or row.status is status)
            and (scope is None or row.scope is scope)
            and (not subject_ref or row.subject_ref == subject_ref)
        )

    def update_status(self, *, tenant_id: str, change: StatusChange) -> int:
        self.status_changes.append(change)
        for row in self.rows:
            if row.id == change.memory_id and row.status is change.expected_status:
                self.rows = tuple(
                    replace(item, status=change.new_status) if item.id == change.memory_id else item
                    for item in self.rows
                )
                return 1
        return 0

    def mark_used(self, *, tenant_id: str, memory_id: str, now: str) -> int:
        self.marks.append((memory_id, now))
        if any(row.id == memory_id for row in self.rows):
            self.rows = tuple(
                replace(item, last_used_at=now) if item.id == memory_id else item for item in self.rows
            )
            return 1
        return 0


def entry(**overrides: object) -> MemoryEntry:
    values: dict[str, object] = {
        "id": MEMORY_ID,
        "tenant_id": TENANT,
        "scope": MemoryScope.USER,
        "subject_ref": SUBJECT,
        "content": "Prefers concise summaries.",
        "provenance_kind": MemoryProvenance.EXPLICIT_USER,
        "status": MemoryStatus.ACTIVE,
    }
    values.update(overrides)
    return MemoryEntry(**values)  # type: ignore[arg-type]


def fabric(store: RecordingStore | None = None) -> tuple[MemoryFabric, RecordingStore]:
    recording = store if store is not None else RecordingStore()
    return MemoryFabric(store=recording, tenant_id=TENANT), recording


# ------------------------------------------------------------------------- boundaries


def test_a_fabric_must_be_bound_to_a_tenant() -> None:
    with pytest.raises(MemoryEntryError) as refusal:
        MemoryFabric(store=RecordingStore(), tenant_id="   ")
    assert refusal.value.rule_id == "memory.tenant"


def test_a_memory_of_another_tenant_is_refused_before_any_write() -> None:
    bound, store = fabric()
    with pytest.raises(MemoryEntryError) as refusal:
        bound.add(entry(tenant_id=TENANT_B))
    assert refusal.value.code == "SCOPE_FORBIDDEN"
    assert refusal.value.rule_id == RULE_TENANT_SCOPE
    assert store.entries == [], "a refused add must not reach the store"


def test_a_duplicate_identity_is_refused_after_the_store_says_so() -> None:
    bound, store = fabric(RecordingStore(insert_result=0))
    with pytest.raises(MemoryEntryError) as refusal:
        bound.add(entry())
    assert refusal.value.code == "CONFLICT_STATE"
    assert refusal.value.rule_id == RULE_STORE
    assert len(store.entries) == 1, "the write was attempted once and not retried"


def test_an_illegal_lifecycle_edge_is_refused_before_any_update() -> None:
    bound, store = fabric(RecordingStore(rows=[entry(status=MemoryStatus.CANDIDATE)]))
    with pytest.raises(MemoryEntryError) as refusal:
        bound.set_status(MEMORY_ID, MemoryStatus.CANDIDATE)
    assert refusal.value.rule_id == "memory.transition"
    assert store.status_changes == [], "the model decides the edge, so no update is attempted"

    deleted = fabric(RecordingStore(rows=[entry(status=MemoryStatus.DELETED)]))[0]
    with pytest.raises(MemoryEntryError) as terminal:
        deleted.set_status(MEMORY_ID, MemoryStatus.ACTIVE)
    assert "terminal" in terminal.value.detail


def test_the_store_guard_is_on_the_state_the_caller_read() -> None:
    """A move that no longer matches changes nothing, so a concurrent actor's decision stands."""
    store = RecordingStore(rows=[entry(status=MemoryStatus.ACTIVE)])
    assert (
        store.update_status(
            tenant_id=TENANT,
            change=StatusChange(MEMORY_ID, MemoryStatus.CANDIDATE, MemoryStatus.ACTIVE),
        )
        == 0
    ), "the caller read `candidate`, the row is `active`: the move must not apply"
    assert store.rows[0].status is MemoryStatus.ACTIVE
    assert (
        store.update_status(
            tenant_id=TENANT,
            change=StatusChange(MEMORY_ID, MemoryStatus.ACTIVE, MemoryStatus.DELETED),
        )
        == 1
    )


def test_a_terminal_memory_cannot_be_reactivated_through_the_fabric() -> None:
    gone = entry(status=MemoryStatus.DELETED)
    bound, store = fabric(RecordingStore(rows=[gone]))
    with pytest.raises(MemoryEntryError) as refusal:
        bound.set_status(MEMORY_ID, MemoryStatus.ACTIVE)
    assert refusal.value.rule_id == "memory.transition"
    assert "terminal" in refusal.value.detail
    assert store.status_changes == [], "the model refused the edge before the store was touched"


def test_a_legal_edge_carries_the_state_the_caller_read() -> None:
    bound, store = fabric(RecordingStore(rows=[entry(status=MemoryStatus.CANDIDATE)]))
    moved = bound.set_status(MEMORY_ID, MemoryStatus.ACTIVE)
    assert moved.status is MemoryStatus.ACTIVE
    assert len(store.status_changes) == 1
    change = store.status_changes[0]
    assert change.expected_status is MemoryStatus.CANDIDATE
    assert change.new_status is MemoryStatus.ACTIVE


def test_the_read_bounds_are_enforced() -> None:
    bound, _store = fabric()
    with pytest.raises(MemoryEntryError) as too_many:
        bound.entries(limit=MAX_LIMIT + 1)
    assert too_many.value.rule_id == RULE_PARAMETERS
    with pytest.raises(MemoryEntryError) as negative:
        bound.entries(offset=-1)
    assert negative.value.rule_id == RULE_PARAMETERS


# ------------------------------------------------------------------------- retrieval


def test_retrieval_excludes_candidates_deleted_and_expired_memories() -> None:
    rows = [
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", status=MemoryStatus.CANDIDATE),
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0BBBBB", status=MemoryStatus.DELETED),
        entry(
            id="mem_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
            status=MemoryStatus.ACTIVE,
            expires_at="2026-01-01T00:00:00Z",
        ),
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0DDDDD", status=MemoryStatus.ACTIVE),
    ]
    bound, _store = fabric(RecordingStore(rows=rows))
    assert [item.id for item in bound.retrievable(NOW)] == ["mem_01J8Z3K6F1N8VQ2X5W9Y0DDDDD"]
    assert len(bound.entries()) == 4, "an expired memory is still stored, just not retrievable"
    assert bound.count(status=MemoryStatus.ACTIVE) == 2


def test_reading_by_subject_and_scope_is_filtered_and_ordered() -> None:
    rows = [
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0BBBBB", subject_ref=SUBJECT),
        entry(id="mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", subject_ref=SUBJECT),
        entry(
            id="mem_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
            scope=MemoryScope.WORKSPACE,
            workspace_id="ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
            subject_ref="ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        ),
    ]
    bound, _store = fabric(RecordingStore(rows=rows))
    assert [item.id for item in bound.entries(subject_ref=SUBJECT)] == [
        "mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        "mem_01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
    ]
    assert [item.id for item in bound.entries(scope=MemoryScope.WORKSPACE)] == [
        "mem_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"
    ]
    assert [item.id for item in bound.entries(limit=1)] == ["mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"]
    assert [item.id for item in bound.entries(limit=1, offset=1)] == ["mem_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"]


def test_an_unknown_identity_is_a_typed_not_found() -> None:
    bound, _store = fabric()
    with pytest.raises(MemoryEntryError) as refusal:
        bound.get(MEMORY_ID)
    assert refusal.value.code == "NOT_FOUND"


def test_marking_used_records_the_instant_and_returns_the_entry() -> None:
    bound, store = fabric(RecordingStore(rows=[entry()]))
    marked = bound.mark_used(MEMORY_ID, NOW)
    assert marked.last_used_at == NOW
    assert store.marks == [(MEMORY_ID, NOW)]
    with pytest.raises(MemoryEntryError) as refusal:
        bound.mark_used(MEMORY_ID, "not-an-instant")
    assert refusal.value.rule_id == "memory.instant"
