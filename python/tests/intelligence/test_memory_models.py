"""The memory entry model and lifecycle (INT-007, DOMAIN.md §11.4).

These assert the properties that keep memory safe to have: it is attributable (a closed provenance
vocabulary and a named subject), it never becomes retrievable by itself (everything starts as a
`candidate` and only `ACTIVE` answers retrieval), an expiry takes it out of retrieval without
deleting it, deletion is terminal, and nothing in the model exists to reconstruct a run — the model
has no position, cursor or pending-effect field at all.
"""

from __future__ import annotations

import dataclasses

import pytest

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

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
SUBJECT = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
MEMORY_ID = f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0AAAAA"


def entry(**overrides: object) -> MemoryEntry:
    values: dict[str, object] = {
        "id": MEMORY_ID,
        "tenant_id": TENANT,
        "scope": MemoryScope.USER,
        "subject_ref": SUBJECT,
        "content": "Prefers concise summaries over long prose.",
        "provenance_kind": MemoryProvenance.EXPLICIT_USER,
        "provenance_ref": "msg_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        "confidence": 0.9,
        "status": MemoryStatus.ACTIVE,
    }
    values.update(overrides)
    return MemoryEntry(**values)  # type: ignore[arg-type]


# ------------------------------------------------------------------------- the model


def test_memory_carries_no_recovery_state() -> None:
    """Memory is not recovery: the model must have nowhere to keep a position or a pending effect."""
    fields = {field.name for field in dataclasses.fields(MemoryEntry)}
    forbidden = {
        "checkpoint",
        "cursor",
        "position",
        "effect_id",
        "effect_record",
        "generation",
        "lease",
        "protocol_state",
        "resume_token",
        "attempt",
        "backoff",
    }
    assert fields.isdisjoint(forbidden), (
        "memory must not be able to hold recovery state (AGENTS.md invariant 7)"
    )


def test_the_declared_shape_is_validated_fail_closed() -> None:
    cases = [
        ("id", {"id": "mem-1"}, "memory.id"),
        ("tenant", {"tenant_id": "  "}, "memory.tenant"),
        ("scope", {"scope": "galaxy"}, "memory.scope"),
        ("subject", {"subject_ref": " "}, "memory.subject"),
        ("content", {"content": "\n  "}, "memory.content"),
        ("provenance", {"provenance_kind": "i_said_so"}, "memory.provenance"),
        ("confidence range", {"confidence": 1.2}, "memory.confidence"),
        ("confidence shape", {"confidence": float("inf")}, "memory.confidence"),
        ("status", {"status": "unknown"}, "memory.status"),
    ]
    for name, overrides, rule in cases:
        with pytest.raises(MemoryEntryError) as refusal:
            entry(**overrides)
        assert refusal.value.rule_id == rule, name
        assert refusal.value.code == "VALIDATION_SCHEMA", name
    assert ID_PREFIX == "mem_", "the identity prefix is the schema's CHECK constraint"


def test_scope_decides_whether_a_workspace_is_named() -> None:
    assert entry(scope=MemoryScope.USER).workspace_id is None
    for scope in (MemoryScope.WORKSPACE, MemoryScope.TEAMMATE):
        assert entry(scope=scope, workspace_id=WORKSPACE).workspace_id == WORKSPACE
        with pytest.raises(MemoryEntryError) as missing:
            entry(scope=scope)
        assert missing.value.rule_id == "memory.scope"
    with pytest.raises(MemoryEntryError) as extra:
        entry(scope=MemoryScope.USER, workspace_id=WORKSPACE)
    assert extra.value.rule_id == "memory.scope"


# ------------------------------------------------------------------------- retrieval


def test_only_active_memory_is_retrievable() -> None:
    now = "2026-09-13T00:00:00Z"
    for status in MemoryStatus:
        item = entry(status=status)
        assert item.retrievable is (status is RETRIEVABLE_STATUS), status.value
        assert item.is_retrievable_at(now) is (status is RETRIEVABLE_STATUS), status.value
    assert RETRIEVABLE_STATUS is MemoryStatus.ACTIVE


def test_an_expired_memory_leaves_retrieval_without_being_deleted() -> None:
    expiring = entry(expires_at="2026-01-01T00:00:00Z")
    assert expiring.expired
    assert not expiring.is_retrievable_at("2026-02-01T00:00:00Z")
    assert expiring.is_retrievable_at("2025-12-31T00:00:00Z")
    assert expiring.status is MemoryStatus.ACTIVE, "an expiry is not a deletion"
    assert expiring.retrievable, "the lifecycle still says active; the clock is what excludes it"

    permanent = entry()
    assert not permanent.expired
    assert permanent.is_retrievable_at("2099-01-01T00:00:00Z")


# ------------------------------------------------------------------------ lifecycle


def test_every_lifecycle_edge_is_legal_or_refused_with_both_ends_named() -> None:
    for state, allowed in TRANSITIONS.items():
        for target in MemoryStatus:
            current = entry(status=state)
            if target in allowed and target is not state:
                assert current.with_status(target).status is target
                continue
            with pytest.raises(MemoryEntryError) as refusal:
                current.with_status(target)
            assert refusal.value.rule_id == "memory.transition"
            assert state.value in refusal.value.detail and target.value in refusal.value.detail


def test_a_memory_never_activates_itself_and_deletion_is_terminal() -> None:
    candidate = entry(status=MemoryStatus.CANDIDATE)
    assert not candidate.retrievable
    active = candidate.with_status(MemoryStatus.ACTIVE)
    assert active.retrievable
    assert active.content == candidate.content, "a promotion is not new content"
    with pytest.raises(MemoryEntryError) as reversal:
        active.with_status(MemoryStatus.CANDIDATE)
    assert "cannot become candidate" in reversal.value.detail

    deleted = active.with_status(MemoryStatus.DELETED)
    assert TRANSITIONS[MemoryStatus.DELETED] == frozenset()
    assert not deleted.retrievable
    with pytest.raises(MemoryEntryError) as revived:
        deleted.with_status(MemoryStatus.ACTIVE)
    assert "terminal" in revived.value.detail


def test_a_use_mark_records_the_instant_and_changes_nothing_else() -> None:
    item = entry()
    marked = item.used_at("2026-09-13T10:00:00Z")
    assert marked.last_used_at == "2026-09-13T10:00:00Z"
    assert marked.content == item.content
    assert marked.status is item.status
    with pytest.raises(MemoryEntryError) as refusal:
        item.used_at("  ")
    assert refusal.value.rule_id == "memory.expiry"


def test_the_provenance_vocabulary_is_closed() -> None:
    assert {kind.value for kind in MemoryProvenance} == {"explicit_user", "verified_run"}
    for kind in MemoryProvenance:
        assert entry(provenance_kind=kind).provenance_kind is kind
