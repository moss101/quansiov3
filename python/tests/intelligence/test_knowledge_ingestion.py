"""Ingestion and the forgetting path, without a database (INT-006 unit 3).

These prove what the owner must guarantee before any row is written: a proposal cannot name an
identity, a tenant or a state; everything ingested starts as a `candidate`; the same claim is not
stored twice; and forgetting a source quarantines in the fabric *before* it touches the derived
index, so an index failure can never lose the authoritative decision. The database-backed suite
proves the same on real rows and a real derived index.
"""

from __future__ import annotations

import dataclasses

import pytest
from test_knowledge_store_boundaries import TENANT, RecordingStore, entry, fabric

from intelligence.knowledge.ingestion import (
    PROVENANCE_APPROVED_SOURCE,
    PROVENANCE_KINDS,
    PROVENANCE_VERIFIED_RUN,
    SOURCE_KIND_KNOWLEDGE,
    KnowledgeProposal,
    forget_entry,
    forget_source,
    ingest,
    ingest_many,
    new_knowledge_id,
)
from intelligence.knowledge.models import (
    ID_PREFIX,
    KnowledgeError,
    KnowledgeStatus,
)


class RecordingDeletion:
    """The INT-011 deletion seam, recording what it was asked to forget."""

    def __init__(self, *, rows: int = 0, failure: Exception | None = None) -> None:
        self.rows = rows
        self.failure = failure
        self.calls: list[tuple[str, str, str]] = []

    def delete_source(self, *, tenant_id: str, source_kind: str, source_ref: str) -> int:
        self.calls.append((tenant_id, source_kind, source_ref))
        if self.failure is not None:
            raise self.failure
        return self.rows


def proposal(**overrides: object) -> KnowledgeProposal:
    values: dict[str, object] = {
        "kind": "runbook",
        "source_kind": "artifact",
        "ref": "art-1",
        "digest": "c" * 64,
        "content_ref": "obj://tenants/tn/artifacts/art-1",
        "confidence": 0.7,
    }
    values.update(overrides)
    return KnowledgeProposal(**values)  # type: ignore[arg-type]


# ------------------------------------------------------------------ what a proposal may be


def test_a_proposal_cannot_name_identity_tenant_scope_or_state() -> None:
    fields = {field.name for field in dataclasses.fields(KnowledgeProposal)}
    assert fields.isdisjoint({"id", "tenant_id", "scope", "status", "superseded_by", "version"}), (
        "identity, tenant, scope and lifecycle are the canonical owner's to decide"
    )
    assert "kind" in fields and "source_kind" in fields


def test_the_provenance_kind_vocabulary_is_closed() -> None:
    assert set(PROVENANCE_KINDS) == {PROVENANCE_APPROVED_SOURCE, PROVENANCE_VERIFIED_RUN}
    with pytest.raises(KnowledgeError) as refusal:
        proposal(provenance_kind="because_i_said_so")
    assert refusal.value.rule_id == "knowledge.provenance_kind"
    with pytest.raises(KnowledgeError) as unnamed:
        proposal(kind="   ")
    assert unnamed.value.rule_id == "knowledge.proposal"


def test_both_evaluated_origins_are_accepted() -> None:
    for kind in PROVENANCE_KINDS:
        parsed = proposal(provenance_kind=kind)
        assert parsed.provenance_kind == kind
        assert parsed.provenance.address == ("artifact", "art-1")
        assert parsed.provenance.digest == "c" * 64


# --------------------------------------------------------------------------- ingestion


def test_ingestion_mints_an_identity_and_stores_a_candidate() -> None:
    bound, store = fabric()
    result = ingest(bound, proposal())
    assert result.created
    stored = result.entry
    assert stored.id.startswith(ID_PREFIX)
    assert stored.tenant_id == TENANT
    assert stored.status is KnowledgeStatus.CANDIDATE, "a model cannot certify its own knowledge"
    assert not stored.retrievable
    assert stored.kind == "runbook"
    assert stored.content_ref == "obj://tenants/tn/artifacts/art-1"
    assert stored.confidence == 0.7
    assert stored.provenance_addresses == (("artifact", "art-1"),)
    assert stored.provenance[0].digest == "c" * 64
    assert len(store.entries) == 1


def test_minted_identities_are_unique_and_time_sortable() -> None:
    minted = [new_knowledge_id() for _ in range(50)]
    assert len(set(minted)) == 50
    assert all(value.startswith(ID_PREFIX) for value in minted)
    # The first ten Crockford characters are the ULID timestamp, so ids never sort before an
    # earlier one; the random field is fresh entropy, so uniqueness is the other half.
    stamps = [value[len(ID_PREFIX) : len(ID_PREFIX) + 10] for value in minted]
    assert stamps == sorted(stamps)


def test_the_same_claim_is_not_stored_twice() -> None:
    bound, store = fabric()
    first = ingest(bound, proposal())
    second = ingest(bound, proposal())
    assert first.created and not second.created
    assert second.entry.id == first.entry.id
    assert second.entry == first.entry
    assert len(store.entries) == 1, "re-proposing the same claim is not a new entry"


def test_a_retired_claim_is_new_knowledge_again() -> None:
    successor = "kn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
    for retired, superseded_by in (
        (KnowledgeStatus.SUPERSEDED, successor),
        (KnowledgeStatus.DELETED, None),
    ):
        existing = entry(
            status=retired,
            superseded_by=superseded_by,
            kind="runbook",
            content_ref="obj://tenants/tn/artifacts/art-1",
        )
        bound, store = fabric(RecordingStore(rows=[existing]))
        result = ingest(bound, proposal())
        assert result.created, retired.value
        assert result.entry.id != existing.id
        assert len(store.entries) == 1


def test_a_claim_of_another_kind_or_address_is_not_a_duplicate() -> None:
    existing = entry(kind="policy", content_ref="obj://tenants/tn/artifacts/art-1")
    bound, _store = fabric(RecordingStore(rows=[existing]))
    assert ingest(bound, proposal()).created
    bound, _store = fabric(RecordingStore(rows=[entry(kind="runbook", content_ref="obj://other")]))
    assert ingest(bound, proposal()).created


def test_ingest_many_reports_every_proposal() -> None:
    bound, _store = fabric()
    results = ingest_many(
        bound,
        [proposal(ref="art-1"), proposal(ref="art-2"), proposal(ref="art-1")],
    )
    assert [result.created for result in results] == [True, True, False]
    assert len({result.entry.id for result in results}) == 2


def test_a_batch_ingest_carries_the_scope_the_owner_decides() -> None:
    from intelligence.knowledge.models import KnowledgeScope

    bound, _store = fabric()
    result = ingest(bound, proposal(), scope=KnowledgeScope.WORKSPACE, workspace_id="ws_1")
    assert result.entry.scope is KnowledgeScope.WORKSPACE
    assert result.entry.workspace_id == "ws_1"


# --------------------------------------------------------------------------- forgetting


def test_forgetting_a_source_quarantines_then_removes_the_index_rows() -> None:
    derived = entry(status=KnowledgeStatus.ACTIVE)
    bound, store = fabric(RecordingStore(rows=[derived]))
    deletion = RecordingDeletion(rows=3)

    outcome = forget_source(bound, deletion, source_kind="artifact", source_ref="art-A")
    assert [item.id for item in outcome.quarantined] == [derived.id]
    assert outcome.quarantined[0].status is KnowledgeStatus.QUARANTINED
    assert outcome.index_rows_removed == 3
    assert outcome.quarantined_rows_removed == 3, "a quarantined entry stops answering retrieval"
    assert outcome.changed
    assert deletion.calls == [
        (TENANT, "artifact", "art-A"),
        (TENANT, SOURCE_KIND_KNOWLEDGE, derived.id),
    ]
    assert len(store.updates) == 1, "the quarantine was recorded in the fabric"


def test_the_quarantine_is_recorded_before_the_index_is_touched() -> None:
    """An unreachable derived index must not cost the authoritative decision."""
    derived = entry(status=KnowledgeStatus.ACTIVE)
    bound, store = fabric(RecordingStore(rows=[derived]))
    deletion = RecordingDeletion(failure=RuntimeError("the vector index is unreachable"))

    with pytest.raises(RuntimeError):
        forget_source(bound, deletion, source_kind="artifact", source_ref="art-A")
    assert len(store.updates) == 1, "the quarantine was applied first and stands"
    assert deletion.calls == [(TENANT, "artifact", "art-A")], "the failure is reported, not hidden"


def test_forgetting_a_source_with_nothing_derived_still_clears_the_index() -> None:
    bound, store = fabric()
    deletion = RecordingDeletion(rows=1)
    outcome = forget_source(bound, deletion, source_kind="artifact", source_ref="art-A")
    assert outcome.quarantined == ()
    assert outcome.index_rows_removed == 1
    assert outcome.quarantined_rows_removed == 0
    assert store.updates == []
    assert outcome.changed, "a source with no derived knowledge still has its own rows"


def test_forgetting_an_entry_withdraws_it_and_forgets_it_as_a_source() -> None:
    withdrawn = entry(status=KnowledgeStatus.ACTIVE)
    bound, store = fabric(RecordingStore(rows=[withdrawn]))
    deletion = RecordingDeletion(rows=2)

    deleted, outcome = forget_entry(bound, deletion, withdrawn.id)
    assert deleted.status is KnowledgeStatus.DELETED
    assert outcome.index_rows_removed == 2
    assert deletion.calls == [(TENANT, SOURCE_KIND_KNOWLEDGE, withdrawn.id)]
    assert len(store.updates) == 1


def test_forgetting_an_already_withdrawn_entry_still_clears_the_index() -> None:
    gone = entry(status=KnowledgeStatus.DELETED)
    bound, store = fabric(RecordingStore(rows=[gone]))
    deletion = RecordingDeletion(rows=1)
    deleted, outcome = forget_entry(bound, deletion, gone.id)
    assert deleted.status is KnowledgeStatus.DELETED
    assert store.updates == [], "deletion is terminal, so it is not re-applied"
    assert outcome.index_rows_removed == 1, "a previous attempt may not have reached the index"
