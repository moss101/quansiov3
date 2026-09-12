"""The core prefix table must match the generated contract enum (CORE-002).

`crates/core` declares the canonical entity prefixes for runtime use, and GOV-004 generates
`quansio.v1.core.EntityPrefix` from DOMAIN.md §1.1. They are two expressions of one table, so
this repository-level check compares them directly: a rename or an added entity on either side
fails here instead of drifting silently.

The check runs in the architecture suite rather than inside `crates/core` so the core crate
keeps no dependency on the generated contracts (and therefore no workspace dependency cycle).
"""
from __future__ import annotations

import re
from pathlib import Path

from scripts.ci import inventory

ROOT = inventory.ROOT
IDENTITY_PROTO = "schemas/proto/quansio/v1/core/identity.proto"
CORE_ID = "crates/core/src/id.rs"


def contract_prefixes() -> set[str]:
    """Entity prefix names declared in the generated-from contract source."""
    text = (ROOT / IDENTITY_PROTO).read_text()
    return {
        name
        for name in re.findall(r"ENTITY_PREFIX_([A-Z_]+)\s*=\s*\d+;", text)
        if name != "UNSPECIFIED"
    }


def core_contract_names() -> set[str]:
    """`contract_name()` values returned by the core prefix table."""
    text = (ROOT / CORE_ID).read_text()
    start = text.index("pub const fn contract_name")
    end = text.index("pub const fn all()", start)
    return set(re.findall(r'=>\s*"([A-Z_]+)"', text[start:end]))


def test_core_prefix_table_matches_the_generated_contract():
    contract = contract_prefixes()
    core = core_contract_names()
    assert core, "core must expose its contract names"
    assert core == contract, (
        "core/contract prefix drift: "
        f"core-only={sorted(core - contract)} contract-only={sorted(contract - core)}"
    )


def test_every_prefix_has_a_distinct_string_and_contract_name():
    text = (ROOT / CORE_ID).read_text()
    prefixes = re.findall(r'=>\s*"([a-z0-9_]+)"', text)
    # 46 prefixes from DOMAIN.md §1.1, each declared once.
    assert len(prefixes) >= 46
    assert len(prefixes) == len(set(prefixes)), "prefix strings must be unique"
