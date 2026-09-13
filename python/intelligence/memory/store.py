"""The durable semantic-memory store (INT-007, DOMAIN.md §11.4).

`public.memory_entries` is the authority for these rows — CORE-001 declares it, with the tenant
filter enforced by forced row-level security and a `WITH CHECK` that refuses a write for another
tenant at the database level as well. This module is the only writer: it never creates a second
memory table, and it never holds recovery state.

Four properties are structural rather than conventional:

* **The tenant is intrinsic.** A [`MemoryFabric`] is bound to one tenant at construction; no method
  accepts a tenant, every statement carries the filter, and the transaction sets the RLS context, so
  a cross-tenant read or write is impossible rather than merely unwritten.
* **Lifecycle is decided by the model.** Every state change goes through `MemoryEntry.with_status`
  and the `UPDATE` is guarded on the state the caller read, so an illegal edge and a concurrent move
  are both refused instead of being applied because SQL can.
* **Retrieval is one predicate, and it includes the clock.** A `candidate` is not retrievable, a
  `deleted` memory is not retrievable, and neither is one whose expiry has passed — the last of which
  the store evaluates in SQL against an instant the *caller* supplies, because the model owns no
  clock and neither does this plane.
* **Nothing is deleted to hide it.** `delete` is the model's terminal lifecycle edge and the row
  stays, which is what keeps the memory auditable and re-proposable.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator, Mapping, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Protocol

from intelligence.memory.models import (
    RULE_SCOPE,
    RULE_STATUS,
    RULE_TENANT,
    MemoryEntry,
    MemoryEntryError,
    MemoryProvenance,
    MemoryScope,
    MemoryStatus,
    require_instant,
)

#: Refusal rules owned by the store, on top of the model's own.
RULE_NOT_FOUND = "memory.not_found"
RULE_STORE = "memory.store"
RULE_TENANT_SCOPE = "memory.tenant_scope"
RULE_PARAMETERS = "memory.parameters"

#: Entries one scoped read returns when the caller names no bound.
DEFAULT_LIMIT = 50
#: Hard ceiling on a scoped read, so one call cannot drain the tenant's memory into a prompt.
MAX_LIMIT = 200

#: Columns every read selects, in one order, so a row decodes the same way everywhere.
_COLUMNS = (
    "id, tenant_id, workspace_id, scope, subject_ref, content, provenance_kind, provenance_ref, "
    "confidence, status, last_used_at, expires_at"
)

#: Statements this module owns. Every one of them filters on the tenant, and none of them names a
#: table other than `memory_entries`: memory is not recovery, so no read here may touch run, step,
#: protocol, checkpoint or effect state. Structural tests pin both invariants.
_SELECT_ONE_SQL = f"""
SELECT {_COLUMNS} FROM memory_entries
WHERE tenant_id = %(tenant_id)s AND id = %(id)s
"""

_SELECT_SCOPED_SQL = f"""
SELECT {_COLUMNS} FROM memory_entries
WHERE tenant_id = %(tenant_id)s
  AND (%(status)s::text IS NULL OR status = %(status)s::text)
  AND (%(scope)s::text IS NULL OR scope = %(scope)s::text)
  AND (%(subject_ref)s::text IS NULL OR subject_ref = %(subject_ref)s::text)
  AND (NOT %(retrievable_only)s::boolean
       OR (status = 'active'
           AND (expires_at IS NULL OR expires_at > %(now)s::timestamptz)))
ORDER BY id
LIMIT %(limit)s OFFSET %(offset)s
"""

_COUNT_SQL = """
SELECT count(*) FROM memory_entries
WHERE tenant_id = %(tenant_id)s
  AND (%(status)s::text IS NULL OR status = %(status)s::text)
  AND (%(scope)s::text IS NULL OR scope = %(scope)s::text)
  AND (%(subject_ref)s::text IS NULL OR subject_ref = %(subject_ref)s::text)
"""

_INSERT_SQL = """
INSERT INTO memory_entries
    (id, tenant_id, workspace_id, scope, subject_ref, content, provenance_kind, provenance_ref,
     confidence, status, last_used_at, expires_at)
VALUES
    (%(id)s, %(tenant_id)s, %(workspace_id)s, %(scope)s, %(subject_ref)s, %(content)s,
     %(provenance_kind)s, %(provenance_ref)s, %(confidence)s, %(status)s, %(last_used_at)s,
     %(expires_at)s)
ON CONFLICT (id) DO NOTHING
"""

_UPDATE_STATUS_SQL = """
UPDATE memory_entries SET status = %(status)s
WHERE tenant_id = %(tenant_id)s AND id = %(id)s AND status = %(expected_status)s
"""

_MARK_USED_SQL = """
UPDATE memory_entries SET last_used_at = %(now)s::timestamptz
WHERE tenant_id = %(tenant_id)s AND id = %(id)s
"""

_TENANT_CONTEXT_SQL = "SELECT set_config('quansio.tenant_id', %(tenant_id)s, true)"

#: Every statement this module owns, for the tenant-filter and no-recovery-table invariants.
STATEMENTS: tuple[str, ...] = (
    _SELECT_ONE_SQL,
    _SELECT_SCOPED_SQL,
    _COUNT_SQL,
    _INSERT_SQL,
    _UPDATE_STATUS_SQL,
    _MARK_USED_SQL,
)

#: The tables this module's SQL may name. Memory is an enrichment, never a way to reconstruct a run.
READ_TABLES: tuple[str, ...] = ("memory_entries",)


class SqlConnection(Protocol):
    """The slice of a DB-API connection this store uses (psycopg satisfies it)."""

    def cursor(self) -> SqlCursor:
        """Open a cursor."""

    def commit(self) -> None:
        """Commit the open transaction."""

    def rollback(self) -> None:
        """Discard the open transaction."""


class SqlCursor(Protocol):
    """The slice of a DB-API cursor this store uses."""

    @property
    def rowcount(self) -> int:
        """Rows the last statement affected."""

    def execute(self, query: str, params: Mapping[str, object] | None = None) -> object:
        """Run one parameterized statement."""

    def fetchall(self) -> Sequence[Sequence[object]]:
        """Every row of the last statement."""

    def close(self) -> None:
        """Close the cursor."""


@dataclass(frozen=True, slots=True)
class StatusChange:
    """One guarded lifecycle move: the state the caller read, and the state it becomes."""

    memory_id: str
    expected_status: MemoryStatus
    new_status: MemoryStatus


class MemoryStore(Protocol):
    """The durable store's contract; every method takes the tenant it applies to."""

    def insert(self, *, entry: MemoryEntry) -> int:
        """Write a new memory; 0 when one with that identity already exists."""

    def get(self, *, tenant_id: str, memory_id: str) -> MemoryEntry:
        """One memory by identity; a typed NOT_FOUND when the tenant does not have it."""

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
        """Memories of one tenant, filtered, bounded and in identity order."""

    def count(
        self,
        *,
        tenant_id: str,
        status: MemoryStatus | None,
        scope: MemoryScope | None,
        subject_ref: str | None,
    ) -> int:
        """How many memories of that tenant match the filter."""

    def update_status(self, *, tenant_id: str, change: StatusChange) -> int:
        """Apply one guarded move; 0 when the row is no longer in the state the caller read."""

    def mark_used(self, *, tenant_id: str, memory_id: str, now: str) -> int:
        """Record that retrieval used one memory; 0 when the tenant has no such memory."""


def _as_int(value: object, column: str) -> int:
    """A decoded column that must be an integer; anything else is refused, not coerced."""
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    if isinstance(value, str):
        return int(value)
    raise MemoryEntryError(
        "INTERNAL", RULE_STORE, f"column {column} decoded as {type(value).__name__}, expected an integer"
    )


def _as_float(value: object, column: str) -> float:
    """A decoded column that must be a number; anything else is refused, not coerced."""
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    if isinstance(value, str):
        return float(value)
    raise MemoryEntryError(
        "INTERNAL", RULE_STORE, f"column {column} decoded as {type(value).__name__}, expected a number"
    )


def _decode_enum[T: (MemoryScope, MemoryProvenance, MemoryStatus)](
    value: object, enum: type[T], rule: str
) -> T:
    """Decode one of the model's states; an unknown value is refused, never coerced."""
    try:
        return enum(str(value))
    except ValueError as error:
        raise MemoryEntryError(
            "INTERNAL", rule, f"stored value {value!r} is not one of {[item.value for item in enum]}"
        ) from error


def _instant_to_text(value: object, column: str) -> str:
    """Render a stored instant in the model's canonical form, or fail closed.

    `TIMESTAMPTZ` comes back as a `datetime`, and the model compares instants lexicographically, so
    the rendering here is what makes a stored instant equal to the one that was written.
    """
    if value is None:
        return ""
    if isinstance(value, str):
        return require_instant(value, field=column)
    isoformat = getattr(value, "isoformat", None)
    if not callable(isoformat):
        raise MemoryEntryError(
            "INTERNAL", RULE_STORE, f"column {column} decoded as {type(value).__name__}, expected an instant"
        )
    rendered = str(isoformat(timespec="seconds"))
    if rendered.endswith("+00:00"):
        rendered = f"{rendered[:-6]}Z"
    return require_instant(rendered, field=column)


def _entry_from_row(row: Sequence[object]) -> MemoryEntry:
    """Reconstruct the domain entry from one row; the model validates it on construction."""
    return MemoryEntry(
        id=str(row[0]),
        tenant_id=str(row[1]),
        workspace_id=str(row[2]) if row[2] is not None else None,
        scope=_decode_enum(row[3], MemoryScope, RULE_SCOPE),
        subject_ref=str(row[4]),
        content=str(row[5]),
        provenance_kind=_decode_enum(row[6], MemoryProvenance, "memory.provenance"),
        provenance_ref=str(row[7]) if row[7] is not None else "",
        confidence=_as_float(row[8], "confidence"),
        status=_decode_enum(row[9], MemoryStatus, RULE_STATUS),
        last_used_at=_instant_to_text(row[10], "last_used_at"),
        expires_at=_instant_to_text(row[11], "expires_at"),
    )


@dataclass(slots=True)
class SqlMemoryStore:
    """The real store: one transaction per operation, tenant context set on every one."""

    connect: Callable[[], SqlConnection]

    @contextmanager
    def _transaction(self, tenant_id: str) -> Iterator[SqlCursor]:
        connection = self.connect()
        cursor = connection.cursor()
        try:
            cursor.execute(_TENANT_CONTEXT_SQL, {"tenant_id": tenant_id})
            yield cursor
        except Exception:
            cursor.close()
            connection.rollback()
            raise
        else:
            cursor.close()
            connection.commit()

    def insert(self, *, entry: MemoryEntry) -> int:
        with self._transaction(entry.tenant_id) as cursor:
            cursor.execute(
                _INSERT_SQL,
                {
                    "id": entry.id,
                    "tenant_id": entry.tenant_id,
                    "workspace_id": entry.workspace_id,
                    "scope": entry.scope.value,
                    "subject_ref": entry.subject_ref,
                    "content": entry.content,
                    "provenance_kind": entry.provenance_kind.value,
                    "provenance_ref": entry.provenance_ref or None,
                    "confidence": float(entry.confidence),
                    "status": entry.status.value,
                    "last_used_at": entry.last_used_at or None,
                    "expires_at": entry.expires_at or None,
                },
            )
            return cursor.rowcount

    def get(self, *, tenant_id: str, memory_id: str) -> MemoryEntry:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(_SELECT_ONE_SQL, {"tenant_id": tenant_id, "id": memory_id})
            rows = cursor.fetchall()
        if not rows:
            raise MemoryEntryError("NOT_FOUND", RULE_NOT_FOUND, f"memory {memory_id!r} is not in this tenant")
        return _entry_from_row(rows[0])

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
        if limit < 1 or limit > MAX_LIMIT:
            raise MemoryEntryError(
                "VALIDATION_SCHEMA",
                RULE_PARAMETERS,
                f"limit must be between 1 and {MAX_LIMIT}, got {limit}",
            )
        if offset < 0:
            raise MemoryEntryError(
                "VALIDATION_SCHEMA", RULE_PARAMETERS, f"offset must not be negative, got {offset}"
            )
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _SELECT_SCOPED_SQL,
                {
                    "tenant_id": tenant_id,
                    "status": status.value if status is not None else None,
                    "scope": scope.value if scope is not None else None,
                    "subject_ref": subject_ref if subject_ref else None,
                    "retrievable_only": retrievable_only,
                    "now": require_instant(now, field="now") if retrievable_only else None,
                    "limit": limit,
                    "offset": offset,
                },
            )
            return tuple(_entry_from_row(row) for row in cursor.fetchall())

    def count(
        self,
        *,
        tenant_id: str,
        status: MemoryStatus | None,
        scope: MemoryScope | None,
        subject_ref: str | None,
    ) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _COUNT_SQL,
                {
                    "tenant_id": tenant_id,
                    "status": status.value if status is not None else None,
                    "scope": scope.value if scope is not None else None,
                    "subject_ref": subject_ref if subject_ref else None,
                },
            )
            rows = cursor.fetchall()
        return _as_int(rows[0][0], "count")

    def update_status(self, *, tenant_id: str, change: StatusChange) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _UPDATE_STATUS_SQL,
                {
                    "tenant_id": tenant_id,
                    "id": change.memory_id,
                    "status": change.new_status.value,
                    "expected_status": change.expected_status.value,
                },
            )
            return cursor.rowcount

    def mark_used(self, *, tenant_id: str, memory_id: str, now: str) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _MARK_USED_SQL,
                {"tenant_id": tenant_id, "id": memory_id, "now": require_instant(now, field="now")},
            )
            return cursor.rowcount


@dataclass(slots=True)
class MemoryFabric:
    """One tenant's memory: the only way to reach `memory_entries`.

    The tenant is bound here and never taken from a caller, so no call site can widen a read or a
    write to another tenant however it is written.
    """

    store: MemoryStore
    tenant_id: str

    def __post_init__(self) -> None:
        if not self.tenant_id.strip():
            raise MemoryEntryError(
                "VALIDATION_SCHEMA", RULE_TENANT, "a memory fabric must be bound to a tenant"
            )

    # -- reads ---------------------------------------------------------------------

    def get(self, memory_id: str) -> MemoryEntry:
        """One memory of this tenant by identity."""
        return self.store.get(tenant_id=self.tenant_id, memory_id=memory_id)

    def entries(
        self,
        *,
        status: MemoryStatus | None = None,
        scope: MemoryScope | None = None,
        subject_ref: str | None = None,
        limit: int = DEFAULT_LIMIT,
        offset: int = 0,
    ) -> tuple[MemoryEntry, ...]:
        """This tenant's memories, filtered and bounded, in identity order."""
        return self.store.scoped(
            tenant_id=self.tenant_id,
            status=status,
            scope=scope,
            subject_ref=subject_ref,
            retrievable_only=False,
            now="",
            limit=limit,
            offset=offset,
        )

    def retrievable(
        self, now: str, *, limit: int = DEFAULT_LIMIT, offset: int = 0
    ) -> tuple[MemoryEntry, ...]:
        """The memories retrieval may serve at `now`: active, and not past their expiry.

        The instant is the caller's, because neither the model nor this plane keeps a clock; the
        store applies it in SQL so an expired memory can never be returned by a read that filtered
        in Python after the fact.
        """
        return self.store.scoped(
            tenant_id=self.tenant_id,
            status=None,
            scope=None,
            subject_ref=None,
            retrievable_only=True,
            now=now,
            limit=limit,
            offset=offset,
        )

    def count(
        self,
        *,
        status: MemoryStatus | None = None,
        scope: MemoryScope | None = None,
        subject_ref: str | None = None,
    ) -> int:
        """How many memories this tenant has (matching the filter)."""
        return self.store.count(tenant_id=self.tenant_id, status=status, scope=scope, subject_ref=subject_ref)

    # -- writes --------------------------------------------------------------------

    def add(self, entry: MemoryEntry) -> MemoryEntry:
        """Add a candidate (or any state the model allows) and read back what was stored."""
        if entry.tenant_id != self.tenant_id:
            raise MemoryEntryError(
                "SCOPE_FORBIDDEN",
                RULE_TENANT_SCOPE,
                f"this fabric is bound to {self.tenant_id!r} and cannot store a memory of "
                f"{entry.tenant_id!r}",
            )
        if self.store.insert(entry=entry) != 1:
            raise MemoryEntryError(
                "CONFLICT_STATE",
                RULE_STORE,
                f"memory {entry.id!r} already exists; delete it or update it instead of overwriting",
            )
        return self.get(entry.id)

    def set_status(self, memory_id: str, status: MemoryStatus) -> MemoryEntry:
        """Move one memory along the lifecycle ladder, or refuse the edge."""
        current = self.get(memory_id)
        moved = current.with_status(status)
        changed = self.store.update_status(
            tenant_id=self.tenant_id,
            change=StatusChange(
                memory_id=memory_id,
                expected_status=current.status,
                new_status=moved.status,
            ),
        )
        if changed != 1:
            raise MemoryEntryError(
                "CONFLICT_STATE",
                RULE_STORE,
                f"memory {memory_id!r} is not in {current.status.value!r} any more; the move was not applied",
            )
        return self.get(memory_id)

    def delete(self, memory_id: str) -> MemoryEntry:
        """The model's terminal lifecycle edge; the row stays, so the memory remains auditable."""
        return self.set_status(memory_id, MemoryStatus.DELETED)

    def mark_used(self, memory_id: str, now: str) -> MemoryEntry:
        """Record that retrieval used one memory at `now`, and read back the marked entry."""
        self.get(memory_id)
        if self.store.mark_used(tenant_id=self.tenant_id, memory_id=memory_id, now=now) != 1:
            raise MemoryEntryError(
                "CONFLICT_STATE", RULE_STORE, f"memory {memory_id!r} could not be marked as used"
            )
        return self.get(memory_id)


def memory_for(store: MemoryStore, *, tenant_id: str) -> MemoryFabric:
    """Bind a memory fabric to one tenant (the only way to obtain one)."""
    return MemoryFabric(store=store, tenant_id=tenant_id)
