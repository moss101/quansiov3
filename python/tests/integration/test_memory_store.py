"""The durable semantic-memory store against real PostgreSQL (INT-007 unit 2).

The plane's unit tests cover the fabric's rules over values; these cover the rows the store actually
writes: identity, provenance, scope and workspace, the instants (`last_used_at`, `expires_at`) that
must survive a round trip through `TIMESTAMPTZ`, the guarded lifecycle, filtered and bounded reads,
`public.memory_entries`' own constraints and forced row-level security. Only the canonical table is
touched, and the suite prepares its scratch database through the repository's seeding tool.

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

from intelligence.memory.models import (
    ID_PREFIX,
    MemoryEntry,
    MemoryEntryError,
    MemoryProvenance,
    MemoryScope,
    MemoryStatus,
)
from intelligence.memory.store import (
    MAX_LIMIT,
    READ_TABLES,
    RULE_NOT_FOUND,
    RULE_PARAMETERS,
    RULE_STORE,
    RULE_TENANT_SCOPE,
    STATEMENTS,
    MemoryFabric,
    SqlMemoryStore,
    StatusChange,
    memory_for,
)

ROOT = Path(__file__).resolve().parents[3]
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()
SEEDER = ROOT / "scripts" / "dev" / "seed_test_database.py"

pytestmark = pytest.mark.skipif(
    not ADMIN_URL,
    reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running",
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
TENANT_B = "tn_01J8Z3K6F1N8VQ2X5W9Y0NNNNN"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
SUBJECT = "usr_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
NOW = "2026-09-13T12:00:00Z"


def seed_scratch_database(name: str, *, tenants: list[str], workspaces: list[str]) -> dict:
    """(Re)create a scratch database through the repository's seeding tool.

    Seeding canonical identities is tooling's job (`scripts/dev/seed_test_database.py`), so neither
    this suite nor any product module contains that write.
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
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


@pytest.fixture(scope="module")
def database() -> Iterator[str]:
    name = f"quansio_pymem_{os.getpid()}"
    seeded = seed_scratch_database(name, tenants=[TENANT, TENANT_B], workspaces=[WORKSPACE])
    try:
        yield seeded["url"]
    finally:
        drop_scratch_database(name)


@pytest.fixture(autouse=True)
def empty_store(database: str) -> Iterator[None]:
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("DELETE FROM memory_entries")
    yield


@pytest.fixture
def fabric(database: str) -> MemoryFabric:
    return memory_for(SqlMemoryStore(lambda: psycopg.connect(database)), tenant_id=TENANT)


def memory_id(suffix: str) -> str:
    return f"{ID_PREFIX}01J8Z3K6F1N8VQ2X5W9Y0{suffix}"


def entry(**overrides: object) -> MemoryEntry:
    values: dict[str, object] = {
        "id": memory_id("AAAAA"),
        "tenant_id": TENANT,
        "scope": MemoryScope.USER,
        "subject_ref": SUBJECT,
        "content": "Prefers concise summaries over long prose.",
        "provenance_kind": MemoryProvenance.EXPLICIT_USER,
        "provenance_ref": "msg_01J8Z3K6F1N8VQ2X5W9Y0MMMMM",
        "confidence": 0.9,
        "status": MemoryStatus.CANDIDATE,
    }
    values.update(overrides)
    return MemoryEntry(**values)  # type: ignore[arg-type]


# --------------------------------------------------------------------- persistence


def test_a_memory_survives_a_write_and_read_back_unchanged(fabric: MemoryFabric) -> None:
    written = entry()
    stored = fabric.add(written)
    assert stored == written, "the round trip must return an equivalent domain entry"
    assert fabric.get(written.id) == written
    assert stored.provenance_kind is MemoryProvenance.EXPLICIT_USER
    assert stored.provenance_ref == "msg_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
    assert stored.confidence == 0.9
    assert stored.status is MemoryStatus.CANDIDATE
    assert stored.scope is MemoryScope.USER
    assert stored.workspace_id is None


def test_instants_survive_the_round_trip_through_timestamptz(fabric: MemoryFabric) -> None:
    written = entry(
        expires_at="2099-01-01T00:00:00Z",
        last_used_at="2026-09-13T10:00:00Z",
    )
    stored = fabric.add(written)
    assert stored.expires_at == written.expires_at, "an instant must read back exactly as written"
    assert stored.last_used_at == written.last_used_at
    assert fabric.get(written.id).expires_at == "2099-01-01T00:00:00Z"


def test_a_workspace_scoped_memory_keeps_its_scope_and_workspace(fabric: MemoryFabric) -> None:
    written = entry(
        scope=MemoryScope.WORKSPACE,
        workspace_id=WORKSPACE,
        subject_ref=WORKSPACE,
    )
    stored = fabric.add(written)
    assert stored.scope is MemoryScope.WORKSPACE
    assert stored.workspace_id == WORKSPACE
    assert [item.id for item in fabric.entries(scope=MemoryScope.WORKSPACE)] == [written.id]


def test_memories_are_listed_filtered_bounded_and_ordered(fabric: MemoryFabric) -> None:
    fabric.add(entry(id=memory_id("BBBBB"), content="Prefers short replies."))
    fabric.add(entry(id=memory_id("AAAAA")))
    fabric.add(
        entry(
            id=memory_id("CCCCC"),
            scope=MemoryScope.WORKSPACE,
            workspace_id=WORKSPACE,
            subject_ref=WORKSPACE,
            status=MemoryStatus.ACTIVE,
        )
    )
    everything = fabric.entries()
    assert [item.id for item in everything] == sorted(item.id for item in everything), (
        "reads are in identity order, so a page is deterministic"
    )
    assert [item.id for item in fabric.entries(subject_ref=SUBJECT)] == [
        memory_id("AAAAA"),
        memory_id("BBBBB"),
    ]
    assert [item.id for item in fabric.entries(limit=2)] == [item.id for item in everything[:2]]
    assert [item.id for item in fabric.entries(limit=1, offset=2)] == [everything[2].id]
    assert fabric.count() == 3
    assert fabric.count(status=MemoryStatus.ACTIVE) == 1


def test_the_boundary_refuses_a_duplicate_identity(fabric: MemoryFabric) -> None:
    fabric.add(entry())
    with pytest.raises(MemoryEntryError) as refusal:
        fabric.add(entry())
    assert refusal.value.code == "CONFLICT_STATE"
    assert refusal.value.rule_id == RULE_STORE
    assert "already exists" in refusal.value.detail


def test_an_unknown_identity_is_a_typed_not_found(fabric: MemoryFabric) -> None:
    with pytest.raises(MemoryEntryError) as refusal:
        fabric.get(memory_id("ZZZZZ"))
    assert refusal.value.code == "NOT_FOUND"
    assert refusal.value.rule_id == RULE_NOT_FOUND


def test_marking_used_persists_only_the_instant(fabric: MemoryFabric) -> None:
    written = fabric.add(entry())
    marked = fabric.mark_used(written.id, NOW)
    assert marked.last_used_at == NOW
    assert marked.content == written.content
    assert marked.status is written.status
    assert fabric.get(written.id).last_used_at == NOW


def test_the_read_bounds_are_enforced(fabric: MemoryFabric) -> None:
    with pytest.raises(MemoryEntryError) as too_many:
        fabric.entries(limit=MAX_LIMIT + 1)
    assert too_many.value.rule_id == RULE_PARAMETERS
    with pytest.raises(MemoryEntryError) as negative:
        fabric.entries(offset=-1)
    assert negative.value.rule_id == RULE_PARAMETERS


# ----------------------------------------------------------------------- lifecycle


def test_the_lifecycle_persists_and_only_active_memory_is_retrievable(
    fabric: MemoryFabric,
) -> None:
    written = fabric.add(entry())
    assert fabric.retrievable(NOW) == ()

    active = fabric.set_status(written.id, MemoryStatus.ACTIVE)
    assert active.status is MemoryStatus.ACTIVE
    assert fabric.get(written.id).status is MemoryStatus.ACTIVE
    assert [item.id for item in fabric.retrievable(NOW)] == [written.id]
    assert active.content == written.content, "a promotion is not new content"

    deleted = fabric.delete(written.id)
    assert deleted.status is MemoryStatus.DELETED
    assert fabric.retrievable(NOW) == ()
    assert fabric.count() == 1, "a deleted memory keeps its row"


def test_an_illegal_transition_is_refused_and_writes_nothing(fabric: MemoryFabric) -> None:
    written = fabric.add(entry())
    with pytest.raises(MemoryEntryError) as refusal:
        fabric.set_status(written.id, MemoryStatus.CANDIDATE)  # candidate -> candidate is not an edge
    assert refusal.value.rule_id == "memory.transition"
    assert fabric.get(written.id).status is MemoryStatus.CANDIDATE

    fabric.set_status(written.id, MemoryStatus.ACTIVE)
    with pytest.raises(MemoryEntryError) as reversal:
        fabric.set_status(written.id, MemoryStatus.CANDIDATE)
    assert "cannot become candidate" in reversal.value.detail
    assert fabric.get(written.id).status is MemoryStatus.ACTIVE

    fabric.delete(written.id)
    with pytest.raises(MemoryEntryError) as terminal:
        fabric.set_status(written.id, MemoryStatus.ACTIVE)
    assert "terminal" in terminal.value.detail
    assert fabric.get(written.id).status is MemoryStatus.DELETED


def test_a_move_that_moved_since_it_was_read_is_refused(fabric: MemoryFabric, database: str) -> None:
    """A concurrent actor's committed move must not be overwritten by a stale expectation."""
    written = fabric.add(entry())
    fabric.set_status(written.id, MemoryStatus.ACTIVE)
    # Another actor deletes the memory, then a stale caller tries to move it from `active`.
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        connection.execute("UPDATE memory_entries SET status = 'deleted' WHERE id = %s", (written.id,))
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")

    assert (
        fabric.store.update_status(
            tenant_id=TENANT,
            change=StatusChange(written.id, MemoryStatus.ACTIVE, MemoryStatus.DELETED),
        )
        == 0
    ), "the row is no longer active, so the stale move applies to nothing"
    assert fabric.get(written.id).status is MemoryStatus.DELETED


def test_an_expired_memory_is_stored_but_not_retrievable(fabric: MemoryFabric) -> None:
    written = fabric.add(entry(expires_at="2026-01-01T00:00:00Z"))
    fabric.set_status(written.id, MemoryStatus.ACTIVE)
    assert fabric.retrievable("2025-06-01T00:00:00Z"), "before the expiry it is retrievable"
    assert fabric.retrievable("2025-12-31T23:59:59Z")
    assert fabric.retrievable("2026-01-01T00:00:00Z") == (), "at the expiry it is not"
    assert fabric.retrievable("2026-06-01T00:00:00Z") == ()
    assert fabric.get(written.id).status is MemoryStatus.ACTIVE, "an expiry is not a deletion"
    assert fabric.count(status=MemoryStatus.ACTIVE) == 1


def test_the_database_owns_the_timestamps(fabric: MemoryFabric, database: str) -> None:
    written = fabric.add(entry())
    fabric.set_status(written.id, MemoryStatus.ACTIVE)
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        created, updated = connection.execute(
            "SELECT created_at, updated_at FROM memory_entries WHERE id = %s", (written.id,)
        ).fetchone()
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
    assert created is not None and updated >= created


# ------------------------------------------------------------------------- isolation


def test_a_fabric_can_only_ever_reach_its_own_tenant(database: str) -> None:
    mine = memory_for(SqlMemoryStore(lambda: psycopg.connect(database)), tenant_id=TENANT)
    theirs = memory_for(SqlMemoryStore(lambda: psycopg.connect(database)), tenant_id=TENANT_B)
    written = mine.add(entry())
    theirs.add(entry(tenant_id=TENANT_B, id=memory_id("BBBBB")))

    assert mine.count() == 1 and theirs.count() == 1, "each fabric sees exactly its own row"
    assert [item.tenant_id for item in mine.entries()] == [TENANT]
    with pytest.raises(MemoryEntryError) as not_mine:
        theirs.get(written.id)
    assert not_mine.value.code == "NOT_FOUND"
    assert [item.id for item in theirs.entries()] == [memory_id("BBBBB")]


def test_a_fabric_refuses_a_memory_of_another_tenant(fabric: MemoryFabric) -> None:
    with pytest.raises(MemoryEntryError) as refusal:
        fabric.add(entry(tenant_id=TENANT_B))
    assert refusal.value.code == "SCOPE_FORBIDDEN"
    assert refusal.value.rule_id == RULE_TENANT_SCOPE
    assert fabric.count() == 0


def test_row_level_security_refuses_a_write_without_a_tenant_context(database: str) -> None:
    """The database's own policy is the second check, not the first."""
    connection = psycopg.connect(database, autocommit=True)
    try:
        connection.execute("SET ROLE quansio_app")
        with pytest.raises(psycopg.errors.InsufficientPrivilege):
            connection.execute(
                "INSERT INTO memory_entries (id, tenant_id, scope, subject_ref, content, "
                "provenance_kind) VALUES (%s, %s, 'user', %s, 'text', 'explicit_user')",
                (memory_id("AAAAA"), TENANT, SUBJECT),
            )
        connection.execute("RESET ROLE")
    finally:
        connection.close()


# ------------------------------------------------------------- corrupted state / invariants


def test_invalid_persisted_state_is_refused_rather_than_coerced(fabric: MemoryFabric, database: str) -> None:
    """A hand-edited row must not read back as a memory the model would never have allowed."""
    written = fabric.add(entry())
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (TENANT,))
        connection.execute("UPDATE memory_entries SET content = '   ' WHERE id = %s", (written.id,))
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
    with pytest.raises(MemoryEntryError) as refusal:
        fabric.get(written.id)
    assert refusal.value.rule_id == "memory.content"


def test_every_statement_is_tenant_scoped_and_names_no_recovery_table() -> None:
    assert STATEMENTS, "the module owns statements to check"
    writes = [statement for statement in STATEMENTS if statement.strip().startswith("INSERT")]
    assert len(writes) == 1, "one insert, which stamps the tenant on every row"
    for statement in STATEMENTS:
        assert "%(tenant_id)s" in statement, statement
        if statement in writes:
            assert "tenant_id," in statement
        else:
            assert "tenant_id = %(tenant_id)s" in statement, statement
        for table in READ_TABLES:
            assert table in statement, statement
        for forbidden in ("runs", "steps", "attempts", "protocol_states", "checkpoints", "effect_records"):
            assert forbidden not in statement, f"{forbidden} is recovery state, not memory: {statement}"
