"""Runtime state tables must agree with the database constraints and with each other (RUN-001).

`crates/server/src/runtime/state_machine/state.rs` and `crates/graph/src/state.rs` both express the
DOMAIN.md §4–§5 state values, and `migrations/0001_canonical_schema.sql` encodes the same values as
`CHECK (status IN (...))` constraints. Three expressions of one table is a drift risk, so this check
compares them mechanically, per family:

* the two Rust modules must expose the same stored values for `RunStatus`, `TurnStatus`, `StepStatus`
  and `AttemptStatus`;
* each Rust set must be accepted by the matching database constraint;
* the database constraint must not accept a value no Rust variant produces (an unreachable state).

`crates/server` cannot depend on `crates/graph` (the graph crate depends on the server's schema module
for tenant context), so the tables cannot be unified by a Rust `use` yet; until that consolidation
lands, this parity check is what keeps them honest. See `HANDOFF.md` "Architecture decisions".
"""
from __future__ import annotations

import re
from pathlib import Path

from scripts.ci import inventory

ROOT = inventory.ROOT
SERVER_STATE = "crates/server/src/runtime/state_machine/state.rs"
GRAPH_STATE = "crates/graph/src/state.rs"
MIGRATION = "migrations/0001_canonical_schema.sql"

# status enum -> (database table, column) whose CHECK constraint must accept its values.
FAMILIES = {
    "RunStatus": ("runs", "status"),
    "TurnStatus": ("turns", "status"),
    "StepStatus": ("steps", "status"),
    "AttemptStatus": ("attempts", "status"),
    "AgentThreadStatus": ("agent_threads", "status"),
}

# AgentThreadStatus has its own owner in each duplicated expression.
ENUM_LOCATIONS = {
    "RunStatus": (SERVER_STATE, GRAPH_STATE),
    "TurnStatus": (SERVER_STATE, GRAPH_STATE),
    "StepStatus": (SERVER_STATE, GRAPH_STATE),
    "AttemptStatus": (SERVER_STATE, GRAPH_STATE),
    "AgentThreadStatus": ("crates/server/src/runtime/agents/mod.rs", GRAPH_STATE),
}


def _read(rel: str) -> str:
    return (ROOT / rel).read_text()


def enum_values(rel: str, enum_name: str) -> set[str]:
    """Stored values of one enum, read from its `impl` block's match arms."""
    text = _read(rel)
    anchor = f"impl {enum_name} {{"
    if anchor not in text:
        return set()
    start = text.index(anchor)
    end = text.index("\n}", start)
    return set(re.findall(r'=>\s*"([A-Za-z_]+)"', text[start:end]))


def db_check_values(table: str, column: str) -> set[str]:
    """Values accepted by the `CHECK (column IN (...))` constraint of a table."""
    migration = _read(MIGRATION)
    start = migration.index(f"CREATE TABLE {table} (")
    end = migration.index(");", start)
    block = migration[start:end]
    match = re.search(rf"CHECK\s*\(\s*{column}\s+IN\s*\(([^)]*)\)", block, re.DOTALL)
    assert match is not None, f"{table}.{column} has no IN (...) check constraint"
    return {value.strip().strip("'") for value in match.group(1).split(",") if value.strip()}


def test_server_and_graph_expose_identical_state_values():
    for enum_name in FAMILIES:
        server_rel, graph_rel = ENUM_LOCATIONS[enum_name]
        server = enum_values(server_rel, enum_name)
        graph = enum_values(graph_rel, enum_name)
        assert server, f"{server_rel} must define {enum_name}"
        assert graph, f"{graph_rel} must define {enum_name}"
        assert server == graph, (
            f"{enum_name} drifted between the runtime and graph owners: "
            f"server-only={sorted(server - graph)} graph-only={sorted(graph - server)}"
        )


def test_state_values_are_accepted_by_the_database_constraint():
    for enum_name, (table, column) in FAMILIES.items():
        allowed = db_check_values(table, column)
        values = enum_values(ENUM_LOCATIONS[enum_name][0], enum_name)
        unexpected = values - allowed
        assert not unexpected, (
            f"{enum_name} produces values the database rejects for {table}.{column}: {sorted(unexpected)}"
        )


def test_database_constraint_has_no_unreachable_state():
    for enum_name, (table, column) in FAMILIES.items():
        allowed = db_check_values(table, column)
        values = enum_values(ENUM_LOCATIONS[enum_name][0], enum_name)
        missing = allowed - values
        assert not missing, (
            f"{table}.{column} allows values no {enum_name} variant produces: {sorted(missing)}"
        )


def test_waiting_states_follow_the_domain_naming():
    """Waiting states are `WAITING_<REASON>` per DOMAIN.md §5.2."""
    values = enum_values(SERVER_STATE, "RunStatus")
    waiting = {value for value in values if value.startswith("WAITING_")}
    assert waiting == {
        "WAITING_APPROVAL",
        "WAITING_QUESTION",
        "WAITING_EVENT",
        "WAITING_TIMER",
        "WAITING_CHILD",
        "WAITING_TAKEOVER",
    }
    terminal = {value for value in values if value in {"SUCCEEDED", "FAILED", "CANCELLED", "BLOCKED_UNRECOVERABLE"}}
    assert len(terminal) == 4
