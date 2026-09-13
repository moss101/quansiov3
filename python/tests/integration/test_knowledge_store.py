"""The durable Knowledge Fabric store against real PostgreSQL (INT-006 unit 2).

The plane's unit tests cover the model's rules over values; these cover the rows the store
actually writes: identity, provenance addressing, the lifecycle guard, one-transaction quarantine,
`public.knowledge_entries`' own constraints and forced row-level security. Only the canonical table
is touched, and the suite seeds its own tenants — it writes no authority row outside `tenants`,
`workspaces` and `knowledge_entries`.

Environment: ``QUANSIO_TEST_POSTGRES_URL`` is the superuser DSN used to create a scratch database
(the same convention as the Rust integration tests). Absent → the suite reports ``BLOCKED_EXTERNAL``
and skips, because the local development stack is not running.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

import psycopg
import pytest

from intelligence.knowledge.models import (
    ID_PREFIX,
    KnowledgeError,
    KnowledgeScope,
    KnowledgeStatus,
    Provenance,
)
from intelligence.knowledge.store import (
    MAX_LIMIT,
    RULE_DERIVED_REF,
    RULE_NOT_FOUND,
    RULE_PARAMETERS,
    RULE_STORE,
    RULE_TENANT_SCOPE,
    STATEMENTS,
    KnowledgeFabric,
    SqlKnowledgeStore,
    StatusUpdate,
    knowledge_for,
)

ROOT = Path(__file__).resolve().parents[3]
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()
SEEDER = ROOT / "scripts" / "dev" / "seed_test_database.py"

pytestmark = pytest.mark.skipif(
    not ADMIN_URL,
    reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running",
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0KKKKK"
RUNBOOK = "Runbook: retention is ninety days for logs."


def seed_scratch_database(name: str, *, tenants: list[str], workspaces: list[str]) -> dict:
    """(Re)create a scratch database with the canonical schema and the identities it needs.

    Seeding canonical identities is tooling's job (`scripts/dev/seed_test_database.py`, the
    counterpart of `scripts/dev/seed` for the dev stack), so neither this suite nor any product
    module contains that write: a Python test may reference an identity the control plane created,
    never create one.
    """
    result = subprocess.run(
        [
            sys.executable,
            str(SEEDER),
            "--admin-url",
            ADMIN_URL,
            "--database",
            name,
            *(f"--tenant={tenant}" for tenant in tenants),
            *(f"--workspace={workspace}" for workspace in workspaces),
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(f"seeding {name} failed:\n{result.stdout}\n{result.stderr}")
    return json.loads(result.stdout.strip().splitlines()[-1])


def drop_scratch_database(name: str) -> None:
    """Remove the scratch database this suite created."""
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


def _scratch_url(name: str) -> str:
    base, _, _ = ADMIN_URL.rpartition("/")
    return f"{base}/{name}"


@pytest.fixture(scope="module")
def database() -> Iterator[str]:
    """A scratch database with the canonical schema and two seeded tenants."""
    name = f"quansio_pykn_{os.getpid()}"
    seeded = seed_scratch_database(name, tenants=[TENANT, TENANT_B], workspaces=[WORKSPACE])
    try:
        yield seeded["url"]
    finally:
        drop_scratch_database(name)


@pytest.fixture(autouse=True)
def empty_fabric(database: str) -> Iterator[None]:
    """Every test starts from an empty fabric, so no test depends on another's cleanup."""
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("DELETE FROM knowledge_entries")
    yield


@pytest.fixture
def fabric(database: str) -> KnowledgeFabric:
    """A tenant-bound fabric over the scratch database."""
    return knowledge_for(SqlKnowledgeStore(lambda: psycopg.connect(database)), tenant_id=TENANT)


def entry_id(suffix: str) -> str:
    return f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0{suffix}"


def provenance(source_kind: str = "artifact", ref: str = "art-1") -> Provenance:
    return Provenance(source_kind=source_kind, ref=ref, digest="b" * 64, retrieved_at="2026-09-13T00:00:00Z")


def candidate(
    *,
    suffix: str = "AAAAA",
    source_kind: str = "artifact",
    ref: str = "art-1",
    kind: str = "runbook",
    scope: KnowledgeScope = KnowledgeScope.TENANT,
    workspace_id: str | None = None,
    confidence: float = 0.8,
    status: KnowledgeStatus = KnowledgeStatus.CANDIDATE,
    tenant_id: str = TENANT,
    provenance_items: tuple[Provenance, ...] | None = None,
):
    from intelligence.knowledge.models import KnowledgeEntry

    return KnowledgeEntry(
        id=entry_id(suffix),
        tenant_id=tenant_id,
        scope=scope,
        workspace_id=workspace_id,
        kind=kind,
        content_ref=f"obj://tenants/{tenant_id}/artifacts/{ref}",
        provenance=provenance_items if provenance_items is not None else (provenance(source_kind, ref),),
        confidence=confidence,
        status=status,
    )


# --------------------------------------------------------------------- persistence


def test_an_entry_survives_a_write_and_read_back_unchanged(fabric: KnowledgeFabric) -> None:
    written = candidate(provenance_items=(provenance("artifact", "art-1"), provenance("thread", "thr-1")))
    stored = fabric.add(written)
    assert stored == written, "the round trip must return an equivalent domain entry"
    assert fabric.get(written.id) == written
    assert stored.provenance_addresses == (("artifact", "art-1"), ("thread", "thr-1"))
    assert stored.confidence == 0.8
    assert stored.version == 1
    assert stored.status is KnowledgeStatus.CANDIDATE


def test_a_workspace_scoped_entry_keeps_its_scope_and_workspace(fabric: KnowledgeFabric) -> None:
    written = candidate(suffix="AAAAA", scope=KnowledgeScope.WORKSPACE, workspace_id=WORKSPACE)
    stored = fabric.add(written)
    assert stored.scope is KnowledgeScope.WORKSPACE
    assert stored.workspace_id == WORKSPACE


def test_entries_are_listed_filtered_and_bounded(fabric: KnowledgeFabric) -> None:
    fabric.add(candidate(suffix="AAAAA", ref="art-1"))
    fabric.add(candidate(suffix="BBBBB", ref="art-2", kind="policy"))
    fabric.add(candidate(suffix="CCCCC", ref="art-3", status=KnowledgeStatus.ACTIVE))
    everything = fabric.entries()
    assert [item.id for item in everything] == sorted(item.id for item in everything), (
        "reads are in identity order, so a page is deterministic"
    )
    assert [item.id for item in fabric.entries(kind="policy")] == [entry_id("BBBBB")]
    assert [item.id for item in fabric.retrievable()] == [entry_id("CCCCC")]
    assert [item.id for item in fabric.entries(limit=2)] == [item.id for item in everything[:2]]
    assert [item.id for item in fabric.entries(limit=1, offset=1)] == [everything[1].id]
    assert fabric.count() == 3
    assert fabric.count(status=KnowledgeStatus.ACTIVE) == 1


def test_an_entry_is_addressable_by_its_provenance(fabric: KnowledgeFabric) -> None:
    fabric.add(candidate(suffix="AAAAA", ref="art-1"))
    fabric.add(candidate(suffix="BBBBB", ref="art-2"))
    shared = candidate(
        suffix="CCCCC",
        ref="art-2",
        provenance_items=(provenance("artifact", "art-1"), provenance("artifact", "art-2")),
    )
    fabric.add(shared)

    assert [item.id for item in fabric.by_provenance("artifact", "art-1")] == [
        entry_id("AAAAA"),
        entry_id("CCCCC"),
    ]
    assert [item.id for item in fabric.by_provenance("artifact", "art-2")] == [
        entry_id("BBBBB"),
        entry_id("CCCCC"),
    ]
    assert fabric.by_provenance("artifact", "art-3") == ()
    with pytest.raises(KnowledgeError) as refusal:
        fabric.by_provenance("artifact", "  ")
    assert refusal.value.rule_id == "knowledge.provenance"


def test_the_boundary_refuses_a_duplicate_identity(fabric: KnowledgeFabric) -> None:
    fabric.add(candidate(suffix="AAAAA"))
    with pytest.raises(KnowledgeError) as refusal:
        fabric.add(candidate(suffix="AAAAA"))
    assert refusal.value.code == "CONFLICT_STATE"
    assert refusal.value.rule_id == RULE_STORE
    assert "already exists" in refusal.value.detail


def test_an_unknown_identity_is_a_typed_not_found(fabric: KnowledgeFabric) -> None:
    with pytest.raises(KnowledgeError) as refusal:
        fabric.get(entry_id("ZZZZZ"))
    assert refusal.value.code == "NOT_FOUND"
    assert refusal.value.rule_id == RULE_NOT_FOUND


def test_the_read_bounds_are_enforced(fabric: KnowledgeFabric) -> None:
    with pytest.raises(KnowledgeError) as too_many:
        fabric.entries(limit=MAX_LIMIT + 1)
    assert too_many.value.rule_id == RULE_PARAMETERS
    with pytest.raises(KnowledgeError) as negative:
        fabric.entries(offset=-1)
    assert negative.value.rule_id == RULE_PARAMETERS


# -------------------------------------------------------------------- lifecycle


def test_the_lifecycle_persists_and_only_active_knowledge_is_retrievable(
    fabric: KnowledgeFabric,
) -> None:
    written = fabric.add(candidate(suffix="AAAAA"))
    assert fabric.retrievable() == ()

    verified = fabric.set_status(written.id, KnowledgeStatus.VERIFIED)
    assert verified.status is KnowledgeStatus.VERIFIED
    assert fabric.get(written.id).status is KnowledgeStatus.VERIFIED

    active = fabric.set_status(written.id, KnowledgeStatus.ACTIVE)
    assert active.status is KnowledgeStatus.ACTIVE
    assert [item.id for item in fabric.retrievable()] == [written.id]
    assert active.version == written.version, "a status change is not new content"

    successor = entry_id("BBBBB")
    fabric.add(candidate(suffix="BBBBB", ref="art-2"))
    superseded = fabric.set_status(written.id, KnowledgeStatus.SUPERSEDED, superseded_by=successor)
    assert superseded.status is KnowledgeStatus.SUPERSEDED
    assert superseded.superseded_by == successor
    assert fabric.retrievable() == (), "superseded knowledge leaves retrieval"
    assert fabric.get(written.id).superseded_by == successor


def test_an_illegal_transition_is_refused_and_writes_nothing(fabric: KnowledgeFabric) -> None:
    written = fabric.add(candidate(suffix="AAAAA"))
    with pytest.raises(KnowledgeError) as refusal:
        fabric.set_status(written.id, KnowledgeStatus.ACTIVE)  # candidate -> active is not an edge
    assert refusal.value.code == "CONFLICT_STATE"
    assert refusal.value.rule_id == "knowledge.transition"
    assert fabric.get(written.id).status is KnowledgeStatus.CANDIDATE

    deleted = fabric.set_status(written.id, KnowledgeStatus.DELETED)
    assert deleted.status is KnowledgeStatus.DELETED
    with pytest.raises(KnowledgeError) as terminal:
        fabric.set_status(written.id, KnowledgeStatus.ACTIVE)
    assert "terminal" in terminal.value.detail
    assert fabric.get(written.id).status is KnowledgeStatus.DELETED


def test_a_row_that_moved_since_it_was_read_is_refused(fabric: KnowledgeFabric, database: str) -> None:
    """A concurrent actor's committed move must not be overwritten by a stale read."""
    written = fabric.add(candidate(suffix="AAAAA"))
    stale = fabric.get(written.id)
    # Another actor verifies the entry between the read and the write.
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        connection.execute("UPDATE knowledge_entries SET status = 'verified' WHERE id = %s", (written.id,))
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")

    assert stale.status is KnowledgeStatus.CANDIDATE
    with pytest.raises(KnowledgeError) as conflict:
        # The stale entry still believes it is a candidate, and candidate -> quarantined is legal,
        # so the refusal can only come from the guard on the state the caller read.
        fabric.store.update_statuses(
            tenant_id=TENANT,
            updates=(
                StatusUpdate(
                    knowledge_id=written.id,
                    expected_status=stale.status,
                    new_status=KnowledgeStatus.QUARANTINED,
                ),
            ),
        )
    assert conflict.value.code == "CONFLICT_STATE"
    assert fabric.get(written.id).status is KnowledgeStatus.VERIFIED, (
        "the concurrent move stands; the stale writer wrote nothing"
    )


def test_deletion_is_a_lifecycle_state_not_a_row_delete(fabric: KnowledgeFabric, database: str) -> None:
    written = fabric.add(candidate(suffix="AAAAA"))
    fabric.set_status(written.id, KnowledgeStatus.VERIFIED)
    deleted = fabric.delete(written.id)
    assert deleted.status is KnowledgeStatus.DELETED
    assert fabric.get(written.id) == deleted, "the row survives, so provenance and audit survive"
    assert fabric.count() == 1
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        rows = connection.execute(
            "SELECT count(*) FROM knowledge_entries WHERE id = %s", (written.id,)
        ).fetchone()
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
    assert rows == (1,), "a deleted entry is retained"


def test_the_database_owns_the_timestamps(fabric: KnowledgeFabric, database: str) -> None:
    written = fabric.add(candidate(suffix="AAAAA"))
    fabric.set_status(written.id, KnowledgeStatus.VERIFIED)
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        created, updated = connection.execute(
            "SELECT created_at, updated_at FROM knowledge_entries WHERE id = %s", (written.id,)
        ).fetchone()
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
    assert created is not None and updated is not None
    assert updated >= created


# ---------------------------------------------------------------- source invalidation


def test_quarantining_a_source_leaves_every_other_entry_alone(fabric: KnowledgeFabric) -> None:
    first = fabric.add(candidate(suffix="AAAAA", ref="art-A"))
    second = fabric.add(candidate(suffix="BBBBB", ref="art-A"))
    third = fabric.add(candidate(suffix="CCCCC", ref="art-B"))
    for item in (first, second, third):
        fabric.set_status(item.id, KnowledgeStatus.VERIFIED)
        fabric.set_status(item.id, KnowledgeStatus.ACTIVE)

    # `third` is derived from another source, so the deletion does not even see it; the derived set
    # it did see is fully accounted for, and the entry itself is asserted unchanged below.
    assert {item.id for item in fabric.by_provenance("artifact", "art-A")} == {first.id, second.id}
    outcome = fabric.quarantine_source("artifact", "art-A")
    assert {item.id for item in outcome.quarantined} == {first.id, second.id}
    assert outcome.unaffected == (), "the deletion only sees the entries derived from the deleted source"
    assert all(item.status is KnowledgeStatus.QUARANTINED for item in outcome.quarantined)
    assert fabric.get(first.id).status is KnowledgeStatus.QUARANTINED
    assert fabric.get(second.id).status is KnowledgeStatus.QUARANTINED
    assert fabric.get(third.id).status is KnowledgeStatus.ACTIVE
    assert [item.id for item in fabric.retrievable()] == [third.id], (
        "quarantined knowledge leaves retrieval; the unrelated entry still answers"
    )
    assert fabric.count() == 3, "nothing was deleted to hide the quarantine"


def test_quarantine_accounts_for_entries_already_out_of_retrieval(fabric: KnowledgeFabric) -> None:
    live = fabric.add(candidate(suffix="AAAAA", ref="art-A"))
    fabric.set_status(live.id, KnowledgeStatus.VERIFIED)
    fabric.set_status(live.id, KnowledgeStatus.ACTIVE)
    superseded = fabric.add(candidate(suffix="BBBBB", ref="art-A"))
    successor = entry_id("CCCCC")
    fabric.add(candidate(suffix="CCCCC", ref="art-C"))
    fabric.set_status(superseded.id, KnowledgeStatus.VERIFIED)
    fabric.set_status(superseded.id, KnowledgeStatus.SUPERSEDED, superseded_by=successor)

    outcome = fabric.quarantine_source("artifact", "art-A")
    assert {item.id for item in outcome.quarantined} == {live.id}
    assert {item.id for item in outcome.unaffected} == {superseded.id}
    assert fabric.get(superseded.id).status is KnowledgeStatus.SUPERSEDED, (
        "an entry already out of retrieval is reported, not rewritten"
    )
    assert fabric.get(live.id).status is KnowledgeStatus.QUARANTINED


def _supersede_concurrently(database: str, *, knowledge_id: str, superseded_by: str) -> None:
    """Another actor's committed move, as the database sees it (not through the fabric)."""
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        connection.execute(
            "UPDATE knowledge_entries SET status = 'superseded', superseded_by = %s WHERE id = %s",
            (superseded_by, knowledge_id),
        )
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")


def test_a_batch_of_moves_is_all_or_nothing(fabric: KnowledgeFabric, database: str) -> None:
    """One stale expectation rolls the whole batch back, so a deletion is never half applied."""
    first = fabric.add(candidate(suffix="AAAAA", ref="art-A"))
    second = fabric.add(candidate(suffix="BBBBB", ref="art-A"))
    for item in (first, second):
        fabric.set_status(item.id, KnowledgeStatus.VERIFIED)
        fabric.set_status(item.id, KnowledgeStatus.ACTIVE)
    _supersede_concurrently(database, knowledge_id=second.id, superseded_by=first.id)

    with pytest.raises(KnowledgeError) as conflict:
        fabric.store.update_statuses(
            tenant_id=TENANT,
            updates=(
                StatusUpdate(first.id, KnowledgeStatus.ACTIVE, KnowledgeStatus.QUARANTINED),
                # Stale: the second entry has already left `active`.
                StatusUpdate(second.id, KnowledgeStatus.ACTIVE, KnowledgeStatus.QUARANTINED),
            ),
        )
    assert conflict.value.code == "CONFLICT_STATE"
    statuses = {item.id: item.status for item in fabric.entries()}
    assert statuses[first.id] is KnowledgeStatus.ACTIVE, (
        "the first move applied, so the batch was not all-or-nothing"
    )
    assert statuses[second.id] is KnowledgeStatus.SUPERSEDED


def test_a_deletion_acts_on_the_state_it_reads(fabric: KnowledgeFabric, database: str) -> None:
    """A source deletion quarantines what is still in force and reports the rest, unchanged."""
    first = fabric.add(candidate(suffix="AAAAA", ref="art-A"))
    second = fabric.add(candidate(suffix="BBBBB", ref="art-A"))
    for item in (first, second):
        fabric.set_status(item.id, KnowledgeStatus.VERIFIED)
        fabric.set_status(item.id, KnowledgeStatus.ACTIVE)
    _supersede_concurrently(database, knowledge_id=second.id, superseded_by=first.id)

    outcome = fabric.quarantine_source("artifact", "art-A")
    assert {item.id for item in outcome.quarantined} == {first.id}
    assert {item.id for item in outcome.unaffected} == {second.id}
    assert fabric.get(first.id).status is KnowledgeStatus.QUARANTINED
    assert fabric.get(second.id).status is KnowledgeStatus.SUPERSEDED


def test_a_quarantine_for_an_unknown_source_changes_nothing(fabric: KnowledgeFabric) -> None:
    fabric.add(candidate(suffix="AAAAA", ref="art-A"))
    outcome = fabric.quarantine_source("artifact", "art-nothing")
    assert outcome.quarantined == ()
    assert not outcome.changed
    assert fabric.count(status=KnowledgeStatus.CANDIDATE) == 1


# -------------------------------------------------------------------- isolation


def test_a_fabric_can_only_ever_reach_its_own_tenant(database: str) -> None:
    mine = knowledge_for(SqlKnowledgeStore(lambda: psycopg.connect(database)), tenant_id=TENANT)
    theirs = knowledge_for(SqlKnowledgeStore(lambda: psycopg.connect(database)), tenant_id=TENANT_B)
    written = mine.add(candidate(suffix="AAAAA", provenance_items=(provenance("artifact", "shared"),)))
    theirs.add(
        candidate(
            suffix="BBBBB",
            tenant_id=TENANT_B,
            provenance_items=(provenance("artifact", "shared"),),
        )
    )

    assert mine.count() == 1 and theirs.count() == 1, "each fabric sees exactly its own row"
    assert [item.tenant_id for item in mine.entries()] == [TENANT]
    with pytest.raises(KnowledgeError) as not_mine:
        theirs.get(written.id)
    assert not_mine.value.code == "NOT_FOUND"

    # A deletion in one tenant's plane cannot touch the other's derived knowledge.
    assert {item.id for item in mine.quarantine_source("artifact", "shared").quarantined} == {written.id}
    assert theirs.by_provenance("artifact", "shared")[0].status is KnowledgeStatus.CANDIDATE


def test_a_fabric_refuses_an_entry_of_another_tenant(fabric: KnowledgeFabric) -> None:
    foreign = candidate(suffix="AAAAA", tenant_id=TENANT_B)
    with pytest.raises(KnowledgeError) as refusal:
        fabric.add(foreign)
    assert refusal.value.code == "SCOPE_FORBIDDEN"
    assert refusal.value.rule_id == RULE_TENANT_SCOPE
    assert fabric.count() == 0


def test_a_fabric_must_be_bound_to_a_tenant(database: str) -> None:
    with pytest.raises(KnowledgeError) as refusal:
        knowledge_for(SqlKnowledgeStore(lambda: psycopg.connect(database)), tenant_id="  ")
    assert refusal.value.rule_id == "knowledge.tenant"


def test_row_level_security_refuses_a_write_without_a_tenant_context(database: str) -> None:
    """The database's own policy is the second check, not the first."""
    connection = psycopg.connect(database, autocommit=True)
    try:
        # The scratch database is seeded through the superuser, which bypasses row-level
        # security; the policy binds the application role, so this runs as that role.
        connection.execute("SET ROLE quansio_app")
        with pytest.raises(psycopg.errors.InsufficientPrivilege):
            connection.execute(
                "INSERT INTO knowledge_entries (id, tenant_id, scope, kind, provenance) "
                "VALUES (%s, %s, 'tenant', 'runbook', '[]'::jsonb)",
                (entry_id("AAAAA"), TENANT),
            )
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT_B,))
        with pytest.raises(psycopg.errors.InsufficientPrivilege):
            connection.execute(
                "INSERT INTO knowledge_entries (id, tenant_id, scope, kind, provenance) "
                "VALUES (%s, %s, 'tenant', 'runbook', '[]'::jsonb)",
                (entry_id("BBBBB"), TENANT),
            )
        connection.execute("RESET ROLE")
    finally:
        connection.close()


# ---------------------------------------------------------------- derived pointer


def test_the_derived_pointer_is_the_index_s_authority(fabric: KnowledgeFabric, database: str) -> None:
    from dataclasses import replace

    written = candidate(suffix="AAAAA")
    with pytest.raises(KnowledgeError) as refusal:
        fabric.add(replace(written, embedding_ref="emb_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"))
    assert refusal.value.rule_id == RULE_DERIVED_REF
    assert fabric.count() == 0


def test_invalid_persisted_state_is_refused_rather_than_coerced(
    fabric: KnowledgeFabric, database: str
) -> None:
    """A hand-edited row must not read back as an entry the model would never have allowed."""
    written = fabric.add(candidate(suffix="AAAAA"))
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        connection.execute(
            "UPDATE knowledge_entries SET provenance = %s::jsonb WHERE id = %s",
            ('{"not": "an address list"}', written.id),
        )
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
    with pytest.raises(KnowledgeError) as refusal:
        fabric.get(written.id)
    assert refusal.value.rule_id == RULE_STORE
    assert "expected a list" in refusal.value.detail


def test_every_statement_is_tenant_scoped() -> None:
    assert STATEMENTS, "the module owns statements to check"
    writes = [statement for statement in STATEMENTS if statement.strip().startswith("INSERT")]
    assert len(writes) == 1, "one insert, which stamps the tenant on every row"
    for statement in STATEMENTS:
        assert "%(tenant_id)s" in statement, statement
        if statement in writes:
            assert "tenant_id," in statement
            continue
        assert "tenant_id = %(tenant_id)s" in statement, statement
