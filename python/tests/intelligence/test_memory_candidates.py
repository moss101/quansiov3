"""The memory candidate gate (INT-007 unit 3).

These prove what the owner decides and what it refuses: a candidate cannot name an identity, a tenant
or a state; the provenance vocabulary is closed; the scope hint is resolved or refused rather than
downgraded (storing a workspace memory as a user one would widen who can see it); re-proposing the
same claim is not a second memory, while re-proposing something that was forgotten is; and a proposal
that cannot be read is refused rather than stored.
"""

from __future__ import annotations

import dataclasses

import pytest
from test_memory_store_boundaries import MEMORY_ID, SUBJECT, TENANT, RecordingStore, entry

from intelligence.memory.candidates import (
    MemoryCandidate,
    StoreMemoryProposals,
    new_memory_id,
    propose,
    resolve_scope,
)
from intelligence.memory.models import (
    ID_PREFIX,
    MemoryEntryError,
    MemoryProvenance,
    MemoryScope,
    MemoryStatus,
)
from intelligence.memory.store import memory_for

WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"


def candidate(**overrides: object) -> MemoryCandidate:
    values: dict[str, object] = {
        "subject_ref": SUBJECT,
        "content": "Prefers concise summaries over long prose.",
        "provenance_kind": MemoryProvenance.EXPLICIT_USER,
        "provenance_ref": "msg_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        "confidence": 0.8,
    }
    values.update(overrides)
    return MemoryCandidate(**values)  # type: ignore[arg-type]


def fabric(store: RecordingStore | None = None):
    recording = store if store is not None else RecordingStore()
    return memory_for(recording, tenant_id=TENANT), recording


# --------------------------------------------------------------- what may be proposed


def test_a_candidate_cannot_name_identity_tenant_or_state() -> None:
    fields = {field.name for field in dataclasses.fields(MemoryCandidate)}
    assert fields.isdisjoint({"id", "tenant_id", "status", "last_used_at"}), (
        "identity, tenant and lifecycle are the canonical owner's to decide"
    )
    assert {"subject_ref", "content", "provenance_kind", "confidence"} <= fields


def test_the_provenance_vocabulary_is_closed_to_evaluated_origins() -> None:
    assert {kind.value for kind in MemoryProvenance} == {"explicit_user", "verified_run"}
    for kind in MemoryProvenance:
        assert propose(fabric()[0], candidate(provenance_kind=kind)).recorded
    with pytest.raises(MemoryEntryError) as refusal:
        candidate(provenance_kind="the_model_thought_so")
    assert refusal.value.rule_id == "memory.candidate"
    assert "explicit user direction or verified work" in refusal.value.detail


def test_a_candidate_needs_a_subject_and_content() -> None:
    for overrides in ({"subject_ref": "  "}, {"content": "\n "}):
        with pytest.raises(MemoryEntryError) as refusal:
            candidate(**overrides)
        assert refusal.value.rule_id == "memory.candidate"
    with pytest.raises(MemoryEntryError) as expiry:
        candidate(expires_at="tomorrow")
    assert expiry.value.rule_id == "memory.instant"


# -------------------------------------------------------------------- scope resolution


def test_the_scope_hint_is_honoured_or_refused_never_downgraded() -> None:
    assert resolve_scope(candidate(scope_hint="user"), workspace_id=WORKSPACE) == (
        MemoryScope.USER,
        None,
    )
    assert resolve_scope(candidate(scope_hint="workspace"), workspace_id=WORKSPACE) == (
        MemoryScope.WORKSPACE,
        WORKSPACE,
    )
    assert resolve_scope(candidate(scope_hint="teammate"), workspace_id=WORKSPACE) == (
        MemoryScope.TEAMMATE,
        WORKSPACE,
    )
    with pytest.raises(MemoryEntryError) as refusal:
        resolve_scope(candidate(scope_hint="workspace"), workspace_id="")
    assert refusal.value.rule_id == "memory.scope_hint"
    assert "needs the workspace" in refusal.value.detail
    with pytest.raises(MemoryEntryError) as unknown:
        candidate(scope_hint="galaxy")
    assert unknown.value.rule_id == "memory.scope_hint"


def test_without_a_hint_the_call_s_workspace_decides() -> None:
    assert resolve_scope(candidate(), workspace_id=WORKSPACE) == (MemoryScope.WORKSPACE, WORKSPACE)
    assert resolve_scope(candidate(), workspace_id="") == (MemoryScope.USER, None)


def test_the_resolved_scope_reaches_the_stored_memory() -> None:
    bound, store = fabric()
    outcome = propose(bound, candidate(scope_hint="teammate"), workspace_id=WORKSPACE)
    assert outcome.entry.scope is MemoryScope.TEAMMATE
    assert outcome.entry.workspace_id == WORKSPACE
    assert store.entries[0].workspace_id == WORKSPACE


# ------------------------------------------------------------------------- identity


def test_a_proposal_is_minted_by_the_owner_and_stored_as_a_candidate() -> None:
    bound, store = fabric()
    outcome = propose(bound, candidate())
    assert outcome.recorded
    stored = outcome.entry
    assert stored.id.startswith(ID_PREFIX)
    assert stored.tenant_id == TENANT
    assert stored.status is MemoryStatus.CANDIDATE, "a model cannot make memory retrievable"
    assert not stored.retrievable
    assert stored.content == "Prefers concise summaries over long prose."
    assert stored.provenance_kind is MemoryProvenance.EXPLICIT_USER
    assert len(store.entries) == 1


def test_minted_identities_are_unique_and_time_sortable() -> None:
    minted = [new_memory_id() for _ in range(50)]
    assert len(set(minted)) == 50
    assert all(value.startswith(ID_PREFIX) for value in minted)
    stamps = [value[len(ID_PREFIX) : len(ID_PREFIX) + 10] for value in minted]
    assert stamps == sorted(stamps)


# ----------------------------------------------------------------------- re-proposal


def test_the_same_claim_is_not_remembered_twice() -> None:
    bound, store = fabric()
    first = propose(bound, candidate())
    second = propose(bound, candidate())
    assert first.recorded and not second.recorded
    assert second.entry.id == first.entry.id
    assert len(store.entries) == 1


def test_a_different_claim_of_the_same_subject_is_a_second_memory() -> None:
    bound, store = fabric()
    propose(bound, candidate())
    assert propose(bound, candidate(content="Prefers short replies.")).recorded
    assert propose(bound, candidate(provenance_ref="msg_other")).recorded
    assert len(store.entries) == 3


def test_a_forgotten_claim_is_new_memory_again() -> None:
    gone = entry(id=MEMORY_ID, status=MemoryStatus.DELETED, content=candidate().content)
    bound, store = fabric(RecordingStore(rows=[gone]))
    outcome = propose(bound, candidate())
    assert outcome.recorded, "re-proposing something forgotten is a new memory"
    assert outcome.entry.id != gone.id
    assert len(store.entries) == 1


# ----------------------------------------------------------------------------- sink


def test_the_store_sink_is_the_production_shape() -> None:
    store = RecordingStore()
    sink = StoreMemoryProposals(store=store)
    outcome = sink.propose(tenant_id=TENANT, workspace_id=WORKSPACE, candidate=candidate())
    assert outcome.recorded
    assert outcome.entry.tenant_id == TENANT
    assert outcome.entry.scope is MemoryScope.WORKSPACE
    assert len(store.entries) == 1
