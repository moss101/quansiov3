"""The durable Knowledge Fabric store (INT-006, DOMAIN.md §11.4).

`public.knowledge_entries` is the authority for these rows — CORE-001 declares it, with the tenant
filter enforced by forced row-level security and a `WITH CHECK` that refuses a write for another
tenant at the database level as well. This module is the only writer: it never creates a second
knowledge table, never builds a second semantic structure (the derived channel is INT-011's) and
never lets a lifecycle state be written that the model in [`models`][intelligence.knowledge.models]
would refuse.

Four properties are structural rather than conventional:

* **The tenant is intrinsic.** A [`KnowledgeFabric`] is bound to one tenant at construction; no
  method accepts a tenant, every statement carries the filter, and the transaction sets the RLS
  context, so a cross-tenant read or write is impossible rather than merely unwritten. The
  database's own policy is the second check, not the first.
* **Lifecycle is decided by the model.** Every state change goes through
  `KnowledgeEntry.with_status`, and the `UPDATE` is guarded on the state the caller read, so an
  illegal edge and a concurrent move are both refused instead of being applied because SQL can.
* **A quarantine is one transaction.** Every entry a source deletion touches is quarantined in a
  single transaction or none of them is, so a concurrent move cannot leave a deletion half applied.
* **Nothing is deleted to hide it.** `delete` is the model's terminal lifecycle edge; the row stays,
  which is what keeps provenance, audit and a future replacement possible.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Iterator, Mapping, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Protocol

from intelligence.knowledge.models import (
    RULE_PROVENANCE,
    RULE_SCOPE,
    RULE_STATUS,
    RULE_TENANT,
    KnowledgeEntry,
    KnowledgeError,
    KnowledgeScope,
    KnowledgeStatus,
    Provenance,
    QuarantineOutcome,
    quarantine_derived,
)

#: Refusal rules owned by the store, on top of the model's own.
RULE_NOT_FOUND = "knowledge.not_found"
RULE_STORE = "knowledge.store"
RULE_TENANT_SCOPE = "knowledge.tenant_scope"
RULE_DERIVED_REF = "knowledge.derived_ref"
RULE_PARAMETERS = "knowledge.parameters"

#: Entries one scoped read returns when the caller names no bound.
DEFAULT_LIMIT = 50
#: Hard ceiling on a scoped read, so one call cannot drain the fabric into memory.
MAX_LIMIT = 200

#: Columns every read selects, in one order, so a row decodes the same way everywhere.
_COLUMNS = (
    "id, tenant_id, workspace_id, scope, kind, content_ref, provenance, confidence, version, "
    "status, superseded_by"
)

#: Statements this module owns. Every one of them filters on the tenant: the table is under forced
#: row-level security and this module adds the explicit predicate as well. A structural test pins
#: that invariant.
_SELECT_ONE_SQL = f"""
SELECT {_COLUMNS} FROM knowledge_entries
WHERE tenant_id = %(tenant_id)s AND id = %(id)s
"""

_SELECT_BY_PROVENANCE_SQL = f"""
SELECT {_COLUMNS} FROM knowledge_entries
WHERE tenant_id = %(tenant_id)s AND provenance @> %(needle)s::jsonb
ORDER BY id
"""

_SELECT_SCOPED_SQL = f"""
SELECT {_COLUMNS} FROM knowledge_entries
WHERE tenant_id = %(tenant_id)s
  AND (%(status)s::text IS NULL OR status = %(status)s::text)
  AND (%(kind)s::text IS NULL OR kind = %(kind)s::text)
ORDER BY id
LIMIT %(limit)s OFFSET %(offset)s
"""

_COUNT_SQL = """
SELECT count(*) FROM knowledge_entries
WHERE tenant_id = %(tenant_id)s
  AND (%(status)s::text IS NULL OR status = %(status)s::text)
"""

_INSERT_SQL = """
INSERT INTO knowledge_entries
    (id, tenant_id, workspace_id, scope, kind, content_ref, provenance, confidence, version, status,
     superseded_by)
VALUES
    (%(id)s, %(tenant_id)s, %(workspace_id)s, %(scope)s, %(kind)s, %(content_ref)s,
     %(provenance)s::jsonb, %(confidence)s, %(version)s, %(status)s, %(superseded_by)s)
ON CONFLICT (id) DO NOTHING
"""

_UPDATE_STATUS_SQL = """
UPDATE knowledge_entries SET status = %(status)s, superseded_by = %(superseded_by)s
WHERE tenant_id = %(tenant_id)s AND id = %(id)s AND status = %(expected_status)s
"""

_TENANT_CONTEXT_SQL = "SELECT set_config('quansio.tenant_id', %(tenant_id)s, true)"

#: Every statement this module owns, for the tenant-filter invariant.
STATEMENTS: tuple[str, ...] = (
    _SELECT_ONE_SQL,
    _SELECT_BY_PROVENANCE_SQL,
    _SELECT_SCOPED_SQL,
    _COUNT_SQL,
    _INSERT_SQL,
    _UPDATE_STATUS_SQL,
)


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
class StatusUpdate:
    """One guarded lifecycle move: the state the caller read, and the state it becomes."""

    knowledge_id: str
    expected_status: KnowledgeStatus
    new_status: KnowledgeStatus
    superseded_by: str | None = None


class KnowledgeStore(Protocol):
    """The durable store's contract; every method takes the tenant it applies to."""

    def insert(self, *, entry: KnowledgeEntry) -> int:
        """Write a new entry; 0 when one with that identity already exists."""

    def get(self, *, tenant_id: str, knowledge_id: str) -> KnowledgeEntry:
        """One entry by identity; a typed NOT_FOUND when the tenant does not have it."""

    def by_provenance(self, *, tenant_id: str, source_kind: str, ref: str) -> tuple[KnowledgeEntry, ...]:
        """Every entry whose provenance names that source, in identity order."""

    def scoped(
        self,
        *,
        tenant_id: str,
        status: KnowledgeStatus | None,
        kind: str | None,
        limit: int,
        offset: int,
    ) -> tuple[KnowledgeEntry, ...]:
        """Entries of one tenant, filtered and bounded, in identity order."""

    def count(self, *, tenant_id: str, status: KnowledgeStatus | None) -> int:
        """How many entries of that tenant (and status) the fabric holds."""

    def update_statuses(self, *, tenant_id: str, updates: Sequence[StatusUpdate]) -> int:
        """Apply every guarded move in one transaction, or none; returns rows changed."""


def _provenance_json(provenance: Sequence[Provenance]) -> str:
    """The JSONB form of an entry's provenance; the model's order is preserved."""
    return json.dumps(
        [
            {
                "source_kind": item.source_kind,
                "ref": item.ref,
                "digest": item.digest,
                "retrieved_at": item.retrieved_at,
            }
            for item in provenance
        ],
        sort_keys=True,
    )


def _decode_provenance(value: object) -> tuple[Provenance, ...]:
    """Decode stored provenance, refusing a row that does not carry a usable address list.

    A corrupted or hand-edited row must not read back as an entry with no provenance, which the
    model would refuse to construct: the refusal is raised here instead of silently dropping the
    address a deletion would have to match on.
    """
    decoded = value
    if isinstance(decoded, (str, bytes, bytearray)):
        try:
            decoded = json.loads(decoded.decode() if isinstance(decoded, (bytes, bytearray)) else decoded)
        except json.JSONDecodeError as error:
            raise KnowledgeError("INTERNAL", RULE_STORE, "stored provenance is not valid JSON") from error
    if not isinstance(decoded, list):
        raise KnowledgeError(
            "INTERNAL",
            RULE_STORE,
            f"stored provenance decoded as {type(decoded).__name__}, expected a list",
        )
    items: list[Provenance] = []
    for entry in decoded:
        if not isinstance(entry, Mapping):
            raise KnowledgeError(
                "INTERNAL", RULE_STORE, "stored provenance carries an entry that is not an object"
            )
        try:
            items.append(
                Provenance(
                    source_kind=str(entry["source_kind"]),
                    ref=str(entry["ref"]),
                    digest=str(entry.get("digest", "")),
                    retrieved_at=str(entry.get("retrieved_at", "")),
                )
            )
        except KeyError as error:
            raise KnowledgeError(
                "INTERNAL", RULE_STORE, f"stored provenance is missing {error.args[0]!r}"
            ) from error
    return tuple(items)


def _decode_enum[T: (KnowledgeScope, KnowledgeStatus)](value: object, enum: type[T], rule: str) -> T:
    """Decode one of the model's states; an unknown value is refused, never coerced."""
    try:
        return enum(str(value))
    except ValueError as error:
        raise KnowledgeError(
            "INTERNAL", rule, f"stored value {value!r} is not one of {[item.value for item in enum]}"
        ) from error


def _as_int(value: object, column: str) -> int:
    """A decoded column that must be an integer; anything else is refused, not coerced."""
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    if isinstance(value, str):
        return int(value)
    raise KnowledgeError(
        "INTERNAL", RULE_STORE, f"column {column} decoded as {type(value).__name__}, expected an integer"
    )


def _as_float(value: object, column: str) -> float:
    """A decoded column that must be a number; anything else is refused, not coerced."""
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    if isinstance(value, str):
        return float(value)
    raise KnowledgeError(
        "INTERNAL", RULE_STORE, f"column {column} decoded as {type(value).__name__}, expected a number"
    )


def _entry_from_row(row: Sequence[object]) -> KnowledgeEntry:
    """Reconstruct the domain entry from one row; the model validates it on construction."""
    return KnowledgeEntry(
        id=str(row[0]),
        tenant_id=str(row[1]),
        workspace_id=str(row[2]) if row[2] is not None else None,
        scope=_decode_enum(row[3], KnowledgeScope, RULE_SCOPE),
        kind=str(row[4]),
        content_ref=str(row[5]) if row[5] is not None else "",
        provenance=_decode_provenance(row[6]),
        confidence=_as_float(row[7], "confidence"),
        version=_as_int(row[8], "version"),
        status=_decode_enum(row[9], KnowledgeStatus, RULE_STATUS),
        superseded_by=str(row[10]) if row[10] is not None else None,
    )


@dataclass(slots=True)
class SqlKnowledgeStore:
    """The real store: one transaction per operation, tenant context set on every one.

    A failed statement rolls its transaction back, so an interrupted lifecycle move or a partially
    applied quarantine never lands.
    """

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

    def insert(self, *, entry: KnowledgeEntry) -> int:
        with self._transaction(entry.tenant_id) as cursor:
            cursor.execute(
                _INSERT_SQL,
                {
                    "id": entry.id,
                    "tenant_id": entry.tenant_id,
                    "workspace_id": entry.workspace_id,
                    "scope": entry.scope.value,
                    "kind": entry.kind,
                    "content_ref": entry.content_ref or None,
                    "provenance": _provenance_json(entry.provenance),
                    "confidence": float(entry.confidence),
                    "version": entry.version,
                    "status": entry.status.value,
                    "superseded_by": entry.superseded_by,
                },
            )
            return cursor.rowcount

    def get(self, *, tenant_id: str, knowledge_id: str) -> KnowledgeEntry:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(_SELECT_ONE_SQL, {"tenant_id": tenant_id, "id": knowledge_id})
            rows = cursor.fetchall()
        if not rows:
            raise KnowledgeError(
                "NOT_FOUND", RULE_NOT_FOUND, f"knowledge entry {knowledge_id!r} is not in this tenant"
            )
        return _entry_from_row(rows[0])

    def by_provenance(self, *, tenant_id: str, source_kind: str, ref: str) -> tuple[KnowledgeEntry, ...]:
        if not source_kind.strip() or not ref.strip():
            raise KnowledgeError(
                "VALIDATION_SCHEMA", RULE_PROVENANCE, "a provenance address needs a kind and a ref"
            )
        needle = json.dumps([{"source_kind": source_kind, "ref": ref}], sort_keys=True)
        with self._transaction(tenant_id) as cursor:
            cursor.execute(_SELECT_BY_PROVENANCE_SQL, {"tenant_id": tenant_id, "needle": needle})
            return tuple(_entry_from_row(row) for row in cursor.fetchall())

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
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_PARAMETERS,
                f"limit must be between 1 and {MAX_LIMIT}, got {limit}",
            )
        if offset < 0:
            raise KnowledgeError(
                "VALIDATION_SCHEMA", RULE_PARAMETERS, f"offset must not be negative, got {offset}"
            )
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _SELECT_SCOPED_SQL,
                {
                    "tenant_id": tenant_id,
                    "status": status.value if status is not None else None,
                    "kind": kind if kind else None,
                    "limit": limit,
                    "offset": offset,
                },
            )
            return tuple(_entry_from_row(row) for row in cursor.fetchall())

    def count(self, *, tenant_id: str, status: KnowledgeStatus | None) -> int:
        with self._transaction(tenant_id) as cursor:
            cursor.execute(
                _COUNT_SQL,
                {"tenant_id": tenant_id, "status": status.value if status is not None else None},
            )
            rows = cursor.fetchall()
        return _as_int(rows[0][0], "count")

    def update_statuses(self, *, tenant_id: str, updates: Sequence[StatusUpdate]) -> int:
        """Apply guarded moves in ONE transaction: a move that no longer matches rolls all back."""
        if not updates:
            return 0
        changed = 0
        with self._transaction(tenant_id) as cursor:
            for update in updates:
                cursor.execute(
                    _UPDATE_STATUS_SQL,
                    {
                        "tenant_id": tenant_id,
                        "id": update.knowledge_id,
                        "status": update.new_status.value,
                        "expected_status": update.expected_status.value,
                        "superseded_by": update.superseded_by,
                    },
                )
                if cursor.rowcount != 1:
                    raise KnowledgeError(
                        "CONFLICT_STATE",
                        RULE_STORE,
                        f"knowledge entry {update.knowledge_id!r} is not in "
                        f"{update.expected_status.value!r} any more; the move was not applied",
                    )
                changed += cursor.rowcount
        return changed


@dataclass(slots=True)
class KnowledgeFabric:
    """One tenant's Knowledge Fabric: the only way to reach `knowledge_entries`.

    The tenant is bound here and never taken from a caller, so no call site can widen a read or a
    write to another tenant however it is written.
    """

    store: KnowledgeStore
    tenant_id: str

    def __post_init__(self) -> None:
        if not self.tenant_id.strip():
            raise KnowledgeError(
                "VALIDATION_SCHEMA", RULE_TENANT, "a knowledge fabric must be bound to a tenant"
            )

    # -- reads ---------------------------------------------------------------------

    def get(self, knowledge_id: str) -> KnowledgeEntry:
        """One entry of this tenant by identity."""
        return self.store.get(tenant_id=self.tenant_id, knowledge_id=knowledge_id)

    def by_provenance(self, source_kind: str, ref: str) -> tuple[KnowledgeEntry, ...]:
        """Every entry derived from one source: the address a source deletion matches on."""
        return self.store.by_provenance(tenant_id=self.tenant_id, source_kind=source_kind, ref=ref)

    def entries(
        self,
        *,
        status: KnowledgeStatus | None = None,
        kind: str | None = None,
        limit: int = DEFAULT_LIMIT,
        offset: int = 0,
    ) -> tuple[KnowledgeEntry, ...]:
        """This tenant's entries, filtered and bounded, in identity order."""
        return self.store.scoped(
            tenant_id=self.tenant_id, status=status, kind=kind, limit=limit, offset=offset
        )

    def retrievable(self, *, limit: int = DEFAULT_LIMIT, offset: int = 0) -> tuple[KnowledgeEntry, ...]:
        """The entries retrieval may serve: only `active` knowledge qualifies (DOMAIN.md §11.4)."""
        return self.entries(status=KnowledgeStatus.ACTIVE, limit=limit, offset=offset)

    def count(self, *, status: KnowledgeStatus | None = None) -> int:
        """How many entries this tenant has (of that status)."""
        return self.store.count(tenant_id=self.tenant_id, status=status)

    # -- writes --------------------------------------------------------------------

    def add(self, entry: KnowledgeEntry) -> KnowledgeEntry:
        """Add a candidate (or any state the model allows) and read back what was stored."""
        if entry.tenant_id != self.tenant_id:
            raise KnowledgeError(
                "SCOPE_FORBIDDEN",
                RULE_TENANT_SCOPE,
                f"this fabric is bound to {self.tenant_id!r} and cannot store an entry of "
                f"{entry.tenant_id!r}",
            )
        if entry.embedding_ref is not None:
            raise KnowledgeError(
                "VALIDATION_SCHEMA",
                RULE_DERIVED_REF,
                "embedding_ref is a derived pointer owned by the vector index (INT-011, "
                "derived.embeddings); this store cannot persist it without becoming a second "
                "derived authority",
            )
        if self.store.insert(entry=entry) != 1:
            raise KnowledgeError(
                "CONFLICT_STATE",
                RULE_STORE,
                f"knowledge entry {entry.id!r} already exists; supersede it instead of overwriting",
            )
        return self.get(entry.id)

    def set_status(
        self,
        knowledge_id: str,
        status: KnowledgeStatus,
        *,
        superseded_by: str | None = None,
    ) -> KnowledgeEntry:
        """Move one entry along the lifecycle ladder, or refuse the edge."""
        current = self.get(knowledge_id)
        moved = current.with_status(status, superseded_by=superseded_by)
        self.store.update_statuses(
            tenant_id=self.tenant_id,
            updates=(
                StatusUpdate(
                    knowledge_id=knowledge_id,
                    expected_status=current.status,
                    new_status=moved.status,
                    superseded_by=moved.superseded_by,
                ),
            ),
        )
        return self.get(knowledge_id)

    def delete(self, knowledge_id: str) -> KnowledgeEntry:
        """The model's terminal lifecycle edge; the row stays, so provenance and audit survive."""
        return self.set_status(knowledge_id, KnowledgeStatus.DELETED)

    def quarantine_source(self, source_kind: str, ref: str) -> QuarantineOutcome:
        """Quarantine the knowledge derived from a deleted source, in one transaction.

        The decision is the model's (`quarantine_derived`); this applies it to the rows. Every
        derived entry is accounted for, and because the writes share one transaction a concurrent
        move leaves the deletion unapplied rather than half applied.
        """
        derived = self.by_provenance(source_kind, ref)
        outcome = quarantine_derived(derived, source_kind=source_kind, ref=ref)
        if not outcome.quarantined:
            return outcome
        originals = {item.id: item for item in derived}
        self.store.update_statuses(
            tenant_id=self.tenant_id,
            updates=tuple(
                StatusUpdate(
                    knowledge_id=item.id,
                    expected_status=originals[item.id].status,
                    new_status=item.status,
                    superseded_by=item.superseded_by,
                )
                for item in outcome.quarantined
            ),
        )
        return QuarantineOutcome(
            quarantined=tuple(self.get(item.id) for item in outcome.quarantined),
            unaffected=outcome.unaffected,
        )


def knowledge_for(store: KnowledgeStore, *, tenant_id: str) -> KnowledgeFabric:
    """Bind a fabric to one tenant (the only way to obtain one)."""
    return KnowledgeFabric(store=store, tenant_id=tenant_id)
