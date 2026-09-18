"""Reproduce the protocol_states first-write race against the real schema.

Usage: QUANSIO_TEST_POSTGRES_URL=... python repro_protocol_state_race.py <variant> <iterations>

variant:
  current      -- the upsert as shipped at 9eb2794: INSERT ... ON CONFLICT (id) DO UPDATE
  run_id       -- the upsert before d6bad3a: INSERT ... ON CONFLICT (run_id) DO UPDATE
  claim        -- the fix: INSERT ... ON CONFLICT DO NOTHING (no target: every unique index
                  arbitrates) followed by UPDATE ... WHERE run_id = $1
  rmw_unlocked -- load, append own pending call, store (claim form) without a row lock
  rmw_locked   -- the same cycle after claim + SELECT ... FOR UPDATE (ProtocolStateStore::lock)

For every iteration a fresh run is created and two transactions perform the run's
first-ever protocol-state write at the same moment. Unique violations are counted per
constraint name.
"""

from __future__ import annotations

import os
import sys
import threading
from collections import Counter
from pathlib import Path

import psycopg

REPO = Path(os.environ.get("QUANSIO_REPO", "."))
ADMIN = os.environ["QUANSIO_TEST_POSTGRES_URL"]
TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD"
USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0DDDDD"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0DDDDD"
WORK_NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD"
AGENT_THREAD = "ath_01J8Z3K6F1N8VQ2X5W9Y0DDDDD"

UPSERT_COLUMNS = (
    "id, tenant_id, run_id, generation, pending_model_call, pending_tool_calls, "
    "pending_approvals, open_questions, waits, browser_control, terminal_sessions, "
    "child_agent_threads, cancellation_requested, cancellation_at, last_compaction_epoch_id"
)
UPSERT_VALUES = "%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s::timestamptz, %s"
UPSERT_SET = (
    "generation = EXCLUDED.generation, pending_model_call = EXCLUDED.pending_model_call, "
    "pending_tool_calls = EXCLUDED.pending_tool_calls, pending_approvals = EXCLUDED.pending_approvals, "
    "open_questions = EXCLUDED.open_questions, waits = EXCLUDED.waits, "
    "browser_control = EXCLUDED.browser_control, terminal_sessions = EXCLUDED.terminal_sessions, "
    "child_agent_threads = EXCLUDED.child_agent_threads, "
    "cancellation_requested = EXCLUDED.cancellation_requested, cancellation_at = EXCLUDED.cancellation_at, "
    "last_compaction_epoch_id = EXCLUDED.last_compaction_epoch_id"
)


def scratch_url(name: str) -> str:
    base, _, _ = ADMIN.rpartition("/")
    return f"{base}/{name}"


def setup(name: str) -> str:
    with psycopg.connect(ADMIN, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")
        admin.execute(f"CREATE DATABASE {name}")
    url = scratch_url(name)
    with psycopg.connect(url) as conn:
        for path in sorted((REPO / "migrations").glob("*.sql")):
            conn.execute(path.read_text())
        conn.execute("INSERT INTO tenants (id, name) VALUES (%s, 'seed')", (TENANT,))
        conn.execute(
            "INSERT INTO users (id, primary_email, display_name) VALUES (%s, %s, 'seed')",
            (USER, f"{USER}@example.com"),
        )
        conn.execute(
            "INSERT INTO workspaces (id, tenant_id, name) VALUES (%s, %s, 'seed')",
            (WORKSPACE, TENANT),
        )
        conn.execute("SELECT set_config('quansio.tenant_id', %s, true)", (TENANT,))
        conn.execute(
            "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) "
            'VALUES (%s, %s, %s, \'objective\', \'seed node\', \'{"kind":"user","id":"usr_seed"}\'::jsonb)',
            (WORK_NODE, TENANT, WORKSPACE),
        )
        conn.execute(
            "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) "
            "VALUES (%s, %s, %s, 'teammate', 1, 'ACTIVE')",
            (AGENT_THREAD, TENANT, WORKSPACE),
        )
        conn.commit()
    return url


def teardown(name: str) -> None:
    with psycopg.connect(ADMIN, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


def create_run(conn: psycopg.Connection, run_id: str) -> None:
    conn.execute("SELECT set_config('quansio.tenant_id', %s, true)", (TENANT,))
    conn.execute(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, "
        "status, trigger_kind) VALUES (%s, %s, %s, %s, %s, 1, 'RUNNING', 'manual')",
        (run_id, TENANT, WORKSPACE, WORK_NODE, AGENT_THREAD),
    )
    conn.commit()


def write(conn: psycopg.Connection, variant: str, run_id: str, marker: str) -> None:
    row_id = "pst_" + run_id.removeprefix("run_")
    params = (
        row_id,
        TENANT,
        run_id,
        1,
        None,
        f'[{{"marker":"{marker}"}}]',
        "[]",
        "[]",
        "[]",
        None,
        "[]",
        "[]",
        False,
        None,
        None,
    )
    conn.execute("SELECT set_config('quansio.tenant_id', %s, true)", (TENANT,))
    if variant == "current":
        conn.execute(
            f"INSERT INTO protocol_states ({UPSERT_COLUMNS}) VALUES ({UPSERT_VALUES}) "
            f"ON CONFLICT (id) DO UPDATE SET {UPSERT_SET}",
            params,
        )
    elif variant == "run_id":
        conn.execute(
            f"INSERT INTO protocol_states ({UPSERT_COLUMNS}) VALUES ({UPSERT_VALUES}) "
            f"ON CONFLICT (run_id) DO UPDATE SET {UPSERT_SET}",
            params,
        )
    elif variant == "claim":
        conn.execute(
            "INSERT INTO protocol_states (id, tenant_id, run_id, generation) VALUES (%s, %s, %s, %s) "
            "ON CONFLICT DO NOTHING",
            (row_id, TENANT, run_id, 1),
        )
        conn.execute(
            "UPDATE protocol_states SET generation = %s, pending_model_call = %s, "
            "pending_tool_calls = %s, pending_approvals = %s, open_questions = %s, waits = %s, "
            "browser_control = %s, terminal_sessions = %s, child_agent_threads = %s, "
            "cancellation_requested = %s, cancellation_at = %s::timestamptz, "
            "last_compaction_epoch_id = %s WHERE run_id = %s",
            (*params[3:], run_id),
        )
    elif variant in ("rmw_unlocked", "rmw_locked"):
        # Read-modify-write the way sync_pending_tool_call does: load, append this
        # writer's pending call, store the whole state (claim + update form).
        import json

        if variant == "rmw_locked":
            conn.execute(
                "INSERT INTO protocol_states (id, tenant_id, run_id, generation) VALUES (%s, %s, %s, %s) "
                "ON CONFLICT DO NOTHING",
                (row_id, TENANT, run_id, 1),
            )
            cur = conn.execute(
                "SELECT pending_tool_calls FROM protocol_states WHERE run_id = %s FOR UPDATE", (run_id,)
            )
        else:
            cur = conn.execute("SELECT pending_tool_calls FROM protocol_states WHERE run_id = %s", (run_id,))
        row = cur.fetchone()
        pending = list(row[0]) if row else []
        pending.append({"marker": marker})
        conn.execute(
            "INSERT INTO protocol_states (id, tenant_id, run_id, generation) VALUES (%s, %s, %s, %s) "
            "ON CONFLICT DO NOTHING",
            (row_id, TENANT, run_id, 1),
        )
        conn.execute(
            "UPDATE protocol_states SET generation = %s, pending_tool_calls = %s WHERE run_id = %s",
            (1, json.dumps(pending), run_id),
        )
    else:
        raise SystemExit(f"unknown variant {variant}")
    conn.commit()


def worker(
    conn: psycopg.Connection,
    variant: str,
    run_id: str,
    marker: str,
    barrier: threading.Barrier,
    errors: list[BaseException],
) -> None:
    """One writer: wait for the other, then perform the write; errors are recorded, not swallowed."""
    try:
        barrier.wait()
        write(conn, variant, run_id, marker)
    except Exception as error:
        errors.append(error)
        conn.rollback()


def main() -> int:
    variant = sys.argv[1]
    iterations = int(sys.argv[2])
    name = f"quansio_repro_{os.getpid()}"
    url = setup(name)
    failures: Counter[str] = Counter()
    other: Counter[str] = Counter()
    try:
        control = psycopg.connect(url)
        writers = [psycopg.connect(url) for _ in range(2)]
        for i in range(iterations):
            run_id = f"run_{i:026d}"
            create_run(control, run_id)
            barrier = threading.Barrier(2)
            errors: list[BaseException] = []
            threads = [
                threading.Thread(target=worker, args=(conn, variant, run_id, marker, barrier, errors))
                for conn, marker in zip(writers, ("a", "b"), strict=True)
            ]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()
            for error in errors:
                if isinstance(error, psycopg.errors.UniqueViolation):
                    failures[error.diag.constraint_name or "?"] += 1
                else:
                    other[type(error).__name__ + ": " + str(error)[:120]] += 1
            with control.cursor() as cur:
                cur.execute("SELECT set_config('quansio.tenant_id', %s, true)", (TENANT,))
                cur.execute("SELECT count(*) FROM protocol_states WHERE run_id = %s", (run_id,))
                (count,) = cur.fetchone()
                control.commit()
                if count != 1:
                    other[f"row count {count}"] += 1
                if variant.startswith("rmw"):
                    cur.execute("SELECT set_config('quansio.tenant_id', %s, true)", (TENANT,))
                    cur.execute("SELECT pending_tool_calls FROM protocol_states WHERE run_id = %s", (run_id,))
                    (pending,) = cur.fetchone()
                    control.commit()
                    markers = sorted(entry["marker"] for entry in pending)
                    if markers != ["a", "b"]:
                        other[f"lost update: final pending markers {markers}"] += 1
    finally:
        teardown(name)
    print(f"variant={variant} iterations={iterations}")
    print(f"unique violations by constraint: {dict(failures)}")
    print(f"other errors: {dict(other)}")
    return 1 if failures or other else 0


if __name__ == "__main__":
    sys.exit(main())
