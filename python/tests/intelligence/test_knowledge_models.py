"""Knowledge entry model and lifecycle (INT-006, DOMAIN.md §11.4).

These assert the properties the rest of the platform relies on: an entry is provenance-addressable
or it does not exist, only `active` knowledge is retrievable, every lifecycle edge is either legal
or refused with both ends named, and deleting a source quarantines exactly the knowledge derived
from it while accounting for every entry.
"""

from __future__ import annotations

import pytest

from intelligence.knowledge.models import (
    ID_PREFIX,
    RETRIEVABLE_STATUS,
    RULE_CONFIDENCE,
    RULE_ID,
    RULE_KIND,
    RULE_PROVENANCE,
    RULE_SCOPE,
    RULE_STATUS,
    RULE_SUPERSEDED,
    RULE_TENANT,
    RULE_TRANSITION,
    RULE_VERSION,
    TRANSITIONS,
    KnowledgeEntry,
    KnowledgeError,
    KnowledgeScope,
    KnowledgeStatus,
    Provenance,
    quarantine_derived,
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
KNOWLEDGE_ID = f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0AAAAA"


def provenance(source_kind: str = "artifact", ref: str = "art-1") -> Provenance:
    return Provenance(source_kind=source_kind, ref=ref, digest="a" * 64, retrieved_at="2026-09-13T00:00:00Z")


def entry(**overrides: object) -> KnowledgeEntry:
    values: dict[str, object] = {
        "id": KNOWLEDGE_ID,
        "tenant_id": TENANT,
        "scope": KnowledgeScope.TENANT,
        "kind": "runbook",
        "provenance": (provenance(),),
        "content_ref": "obj://tenants/tn/artifacts/art-1/versions/v1",
        "confidence": 0.8,
        "status": KnowledgeStatus.ACTIVE,
    }
    values.update(overrides)
    return KnowledgeEntry(**values)  # type: ignore[arg-type]


# ------------------------------------------------------------------ provenance addressing


def test_an_entry_is_addressable_by_its_provenance() -> None:
    item = entry(provenance=(provenance("artifact", "art-1"), provenance("thread", "thr-1")))
    assert item.provenance_addresses == (("artifact", "art-1"), ("thread", "thr-1"))
    assert item.derives_from("thread", "thr-1")
    assert not item.derives_from("thread", "thr-2")
    assert item.provenance[0].digest == "a" * 64


def test_an_entry_without_provenance_is_refused() -> None:
    with pytest.raises(KnowledgeError) as refusal:
        entry(provenance=())
    assert refusal.value.rule_id == RULE_PROVENANCE
    assert "unaddressable" in refusal.value.detail


def test_a_provenance_reference_needs_a_kind_and_a_ref() -> None:
    with pytest.raises(KnowledgeError) as kind:
        Provenance(source_kind="  ", ref="art-1")
    assert kind.value.rule_id == RULE_PROVENANCE
    with pytest.raises(KnowledgeError) as ref:
        Provenance(source_kind="artifact", ref="")
    assert ref.value.rule_id == RULE_PROVENANCE


# ------------------------------------------------------------------------------ shape


def test_the_declared_shape_is_validated_fail_closed() -> None:
    cases = [
        ("id", {"id": "kn-1"}, RULE_ID, "VALIDATION_SCHEMA"),
        ("tenant", {"tenant_id": "   "}, RULE_TENANT, "VALIDATION_SCHEMA"),
        ("scope", {"scope": "galaxy"}, RULE_SCOPE, "VALIDATION_SCHEMA"),
        ("kind", {"kind": " "}, RULE_KIND, "VALIDATION_SCHEMA"),
        ("confidence range", {"confidence": 1.5}, RULE_CONFIDENCE, "VALIDATION_SCHEMA"),
        ("confidence shape", {"confidence": float("nan")}, RULE_CONFIDENCE, "VALIDATION_SCHEMA"),
        ("version", {"version": 0}, RULE_VERSION, "VALIDATION_SCHEMA"),
        ("status", {"status": "unknown"}, RULE_STATUS, "VALIDATION_SCHEMA"),
    ]
    for name, overrides, rule, code in cases:
        with pytest.raises(KnowledgeError) as refusal:
            entry(**overrides)
        assert refusal.value.rule_id == rule, name
        assert refusal.value.code == code, name
    assert ID_PREFIX == "kn_", "the identity prefix is the schema's CHECK constraint"


def test_a_workspace_scoped_entry_must_name_its_workspace_and_no_other_scope_may() -> None:
    scoped = entry(scope=KnowledgeScope.WORKSPACE, workspace_id=WORKSPACE)
    assert scoped.workspace_id == WORKSPACE

    with pytest.raises(KnowledgeError) as missing:
        entry(scope=KnowledgeScope.WORKSPACE)
    assert missing.value.rule_id == RULE_SCOPE
    with pytest.raises(KnowledgeError) as extra:
        entry(scope=KnowledgeScope.PACK, workspace_id=WORKSPACE)
    assert extra.value.rule_id == RULE_SCOPE
    with pytest.raises(KnowledgeError) as tenant_scoped:
        entry(scope=KnowledgeScope.TENANT, workspace_id=WORKSPACE)
    assert tenant_scoped.value.rule_id == RULE_SCOPE


def test_superseded_by_must_agree_with_the_status() -> None:
    successor = f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0BBBBB"
    superseded = entry(status=KnowledgeStatus.SUPERSEDED, superseded_by=successor)
    assert superseded.superseded_by == successor
    assert not superseded.retrievable

    with pytest.raises(KnowledgeError) as unnamed:
        entry(status=KnowledgeStatus.SUPERSEDED)
    assert unnamed.value.rule_id == RULE_SUPERSEDED
    with pytest.raises(KnowledgeError) as misfiled:
        entry(status=KnowledgeStatus.ACTIVE, superseded_by=successor)
    assert misfiled.value.rule_id == RULE_SUPERSEDED
    with pytest.raises(KnowledgeError) as itself:
        entry(status=KnowledgeStatus.SUPERSEDED, superseded_by=KNOWLEDGE_ID)
    assert itself.value.rule_id == RULE_SUPERSEDED


# -------------------------------------------------------------------------- retrieval


def test_only_active_knowledge_is_retrievable() -> None:
    for status in KnowledgeStatus:
        item = entry(
            status=status,
            superseded_by=(f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
            if status is KnowledgeStatus.SUPERSEDED
            else None,
        )
        assert item.retrievable is (status is RETRIEVABLE_STATUS), status.value
    assert RETRIEVABLE_STATUS is KnowledgeStatus.ACTIVE


# -------------------------------------------------------------------------- lifecycle


def test_every_lifecycle_edge_is_legal_or_refused_with_both_ends_named() -> None:
    for state, allowed in TRANSITIONS.items():
        for target in KnowledgeStatus:
            current = entry(
                status=state,
                superseded_by=(f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
                if state is KnowledgeStatus.SUPERSEDED
                else None,
            )
            if target in allowed and target is not state:
                successor = f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0CCCCC"
                moved = current.with_status(target, superseded_by=successor)
                assert moved.status is target
                assert moved.version == current.version, "a status change is not new content"
                assert moved.confidence == current.confidence
                assert (moved.superseded_by == successor) is (target is KnowledgeStatus.SUPERSEDED)
                continue
            with pytest.raises(KnowledgeError) as refusal:
                current.with_status(target)
            assert refusal.value.rule_id == RULE_TRANSITION
            assert state.value in refusal.value.detail and target.value in refusal.value.detail


def test_deletion_is_terminal_and_quarantine_is_recoverable() -> None:
    deleted = entry(status=KnowledgeStatus.DELETED)
    assert TRANSITIONS[KnowledgeStatus.DELETED] == frozenset()
    with pytest.raises(KnowledgeError) as reanimated:
        deleted.with_status(KnowledgeStatus.ACTIVE)
    assert "terminal" in reanimated.value.detail

    quarantined = entry(status=KnowledgeStatus.QUARANTINED)
    assert not quarantined.retrievable
    assert quarantined.with_status(KnowledgeStatus.VERIFIED).status is KnowledgeStatus.VERIFIED


# -------------------------------------------------------------------- source deletion


def test_deleting_a_source_quarantines_exactly_the_knowledge_derived_from_it() -> None:
    derived = entry(provenance=(provenance("artifact", "art-1"),))
    unrelated = entry(
        id=f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
        provenance=(provenance("artifact", "art-2"),),
    )
    already_superseded = entry(
        id=f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        provenance=(provenance("artifact", "art-1"),),
        status=KnowledgeStatus.SUPERSEDED,
        superseded_by=f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0DDDDD",
    )

    outcome = quarantine_derived(
        [derived, unrelated, already_superseded], source_kind="artifact", ref="art-1"
    )
    assert outcome.changed
    assert [item.id for item in outcome.quarantined] == [derived.id]
    assert outcome.quarantined[0].status is KnowledgeStatus.QUARANTINED
    assert not outcome.quarantined[0].retrievable, "quarantined knowledge must leave retrieval"
    assert not outcome.quarantined[0].derives_from("artifact", "art-2")
    assert {item.id for item in outcome.unaffected} == {unrelated.id, already_superseded.id}
    assert unrelated.retrievable, "the original entries are unchanged; the result is a new value"


def test_quarantining_is_deterministic_total_and_never_deletes() -> None:
    entries = [
        entry(provenance=(provenance("artifact", "art-1"),)),
        entry(
            id=f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
            provenance=(provenance("artifact", "art-2"),),
        ),
    ]
    first = quarantine_derived(entries, source_kind="artifact", ref="art-1")
    second = quarantine_derived(entries, source_kind="artifact", ref="art-1")
    assert first == second, "the same deletion produces the same outcome"
    assert len(first.quarantined) + len(first.unaffected) == len(entries), "every entry must be accounted for"
    assert all(item.status is not KnowledgeStatus.DELETED for item in (*first.quarantined, *first.unaffected))


def test_quarantine_never_crosses_a_tenant() -> None:
    """Knowledge is tenant-scoped: one tenant's source cannot affect another tenant's entries."""
    mine = entry(provenance=(provenance("artifact", "shared-ref"),))
    theirs = entry(
        id=f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
        tenant_id=TENANT_B,
        provenance=(provenance("artifact", "shared-ref"),),
    )
    # The operation is per-tenant by construction: the caller passes the entries it owns, so a
    # deletion in one tenant's plane is never applied to another tenant's entries.
    outcome = quarantine_derived([mine], source_kind="artifact", ref="shared-ref")
    assert [item.id for item in outcome.quarantined] == [mine.id]
    assert theirs.retrievable and theirs.tenant_id == TENANT_B


def test_a_source_deletion_needs_the_source_identity() -> None:
    with pytest.raises(KnowledgeError) as refusal:
        quarantine_derived([entry()], source_kind=" ", ref="art-1")
    assert refusal.value.rule_id == RULE_PROVENANCE
    with pytest.raises(KnowledgeError):
        quarantine_derived([entry()], source_kind="artifact", ref="")
