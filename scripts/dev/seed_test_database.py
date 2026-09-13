#!/usr/bin/env python3
"""Prepare a scratch PostgreSQL database for the intelligence plane's integration tests.

Integration tests in `python/tests/integration/` need a database with the canonical schema *and*
canonical identities in it: `knowledge_entries.tenant_id`, for example, references `tenants`, so a
suite cannot write a single knowledge row without a tenant that already exists.

Seeding identities is tooling's job, not the product's or a test's. This tool is the counterpart of
`scripts/dev/seed` for a scratch database: `scripts/dev/seed` seeds the dev stack's identities
through `psql`, and this seeds a test database the same way — one reviewable artifact instead of
authority SQL duplicated across every suite. `AGENTS.md` invariant 11 puts canonical identity in the
Rust control plane, which is why no Python *product* module and no test contains that write: the
intelligence plane reads and writes its own tables (`knowledge_entries`, `derived.embeddings`) and
only ever references an identity somebody else created.

Usage (from the repository root):

    uv run --project python python scripts/dev/seed_test_database.py \
        --admin-url postgres://user:pass@host:port/postgres \
        --database quansio_test_example \
        --tenant tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA \
        --workspace ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA

Prints one JSON object on stdout with the database URL and the identities it created, so a suite can
consume exactly what was seeded rather than restating it.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

import psycopg

ROOT = Path(__file__).resolve().parents[2]
MIGRATIONS = ROOT / "migrations"

#: The canonical identity shapes (DOMAIN.md §1.1). A malformed id is refused here rather than
#: inserted, so a typo cannot create an identity the rest of the platform would not recognise.
TENANT_ID_RE = re.compile(r"^tn_[0-9A-HJKMNP-TV-Z]{26}$")
WORKSPACE_ID_RE = re.compile(r"^ws_[0-9A-HJKMNP-TV-Z]{26}$")
DATABASE_NAME_RE = re.compile(r"^[a-z][a-z0-9_]{0,62}$")


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="seed_test_database", description=__doc__)
    parser.add_argument("--admin-url", required=True, help="DSN of a database that can CREATE DATABASE")
    parser.add_argument("--database", required=True, help="scratch database to (re)create")
    parser.add_argument("--tenant", required=True, action="append", help="tenant id to seed")
    parser.add_argument(
        "--workspace", action="append", default=[], help="workspace id to seed in the first tenant"
    )
    parser.add_argument(
        "--keep", action="store_true", help="refuse to drop an existing database instead of replacing it"
    )
    return parser.parse_args(argv)


def _scratch_url(admin_url: str, database: str) -> str:
    base, _, _ = admin_url.rpartition("/")
    return f"{base}/{database}"


def _create_database(admin_url: str, database: str, *, keep: bool) -> str:
    if not DATABASE_NAME_RE.match(database):
        raise SystemExit(f"refusing to create database {database!r}: not a scratch database name")
    with psycopg.connect(admin_url, autocommit=True) as connection:
        exists = connection.execute(
            "SELECT 1 FROM pg_database WHERE datname = %s", (database,)
        ).fetchone()
        if exists and keep:
            raise SystemExit(f"database {database!r} already exists and --keep was given")
        connection.execute(f'DROP DATABASE IF EXISTS "{database}" WITH (FORCE)')
        connection.execute(f'CREATE DATABASE "{database}"')
    return _scratch_url(admin_url, database)


def _migrate(url: str) -> list[str]:
    applied: list[str] = []
    with psycopg.connect(url, autocommit=True) as connection:
        for path in sorted(MIGRATIONS.glob("*.sql")):
            connection.execute(path.read_text())
            applied.append(path.name)
    return applied


def _seed(url: str, tenants: list[str], workspaces: list[str]) -> tuple[list[str], list[str]]:
    """Seed the identities, in the tenant's own row-security context as the schema requires."""
    with psycopg.connect(url, autocommit=True) as connection:
        for tenant in tenants:
            if not TENANT_ID_RE.match(tenant):
                raise SystemExit(f"refusing to seed tenant {tenant!r}: not a canonical tenant id")
            connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (tenant,))
            connection.execute(
                "INSERT INTO tenants (id, name, personal) VALUES (%s, %s, true) "
                "ON CONFLICT (id) DO NOTHING",
                (tenant, f"test tenant {tenant}"),
            )
        connection.execute("SELECT set_config('quansio.tenant_id', %s, false)", (tenants[0],))
        for workspace in workspaces:
            if not WORKSPACE_ID_RE.match(workspace):
                raise SystemExit(
                    f"refusing to seed workspace {workspace!r}: not a canonical workspace id"
                )
            connection.execute(
                "INSERT INTO workspaces (id, tenant_id, name) VALUES (%s, %s, %s) "
                "ON CONFLICT (id) DO NOTHING",
                (workspace, tenants[0], f"test workspace {workspace}"),
            )
        connection.execute("SELECT set_config('quansio.tenant_id', '', false)")
    return tenants, workspaces


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    if not args.tenant:
        raise SystemExit("at least one --tenant is required")
    url = _create_database(args.admin_url, args.database, keep=args.keep)
    applied = _migrate(url)
    tenants, workspaces = _seed(url, list(args.tenant), list(args.workspace))
    print(
        json.dumps(
            {
                "database": args.database,
                "url": url,
                "tenants": tenants,
                "workspaces": workspaces,
                "migrations": applied,
            }
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
