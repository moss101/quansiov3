"""The Rust data-lifecycle control applies only edges the intelligence plane's models have.

`crates/server/src/control/data_lifecycle/intelligence.rs` (APP-011) tombstones a knowledge or
memory entry on a person's behalf: `→ deleted` and `→ superseded`. The lifecycle *model* stays
here (`knowledge/models.py`, `memory/models.py`); this test pins the Rust edge tables and the
derived-index source kinds to the models so the two owners cannot drift apart. It reads the
Rust source as text — no Rust toolchain is needed to run it.
"""

from __future__ import annotations

import re
from pathlib import Path

from intelligence.knowledge.ingestion import SOURCE_KIND_KNOWLEDGE
from intelligence.knowledge.models import TRANSITIONS as KNOWLEDGE_TRANSITIONS
from intelligence.knowledge.models import KnowledgeStatus
from intelligence.memory.models import TRANSITIONS as MEMORY_TRANSITIONS
from intelligence.memory.models import MemoryStatus
from intelligence.memory.retrieval import SOURCE_KIND_MEMORY

REPO_ROOT = Path(__file__).resolve().parents[3]
RUST_CONTROL = REPO_ROOT / "crates" / "server" / "src" / "control" / "data_lifecycle" / "intelligence.rs"

_ARRAY = re.compile(r"pub const (?P<name>[A-Z_]+): \[&str; \d+\] =\s*\[(?P<items>[^\]]*)\];", re.S)
_STR = re.compile(r'pub const (?P<name>SOURCE_KIND_[A-Z]+): &str = "(?P<value>[^"]+)";')


def _rust_arrays() -> dict[str, frozenset[str]]:
    text = RUST_CONTROL.read_text(encoding="utf-8")
    found = {
        match.group("name"): frozenset(re.findall(r'"([a-z_]+)"', match.group("items")))
        for match in _ARRAY.finditer(text)
    }
    assert found, f"no edge tables in {RUST_CONTROL}"
    return found


def _rust_strings() -> dict[str, str]:
    text = RUST_CONTROL.read_text(encoding="utf-8")
    return {match.group("name"): match.group("value") for match in _STR.finditer(text)}


def _sources_of[T: (KnowledgeStatus, MemoryStatus)](
    transitions: dict[T, frozenset[T]], target: T
) -> frozenset[str]:
    return frozenset(state.value for state, allowed in transitions.items() if target in allowed)


def test_rust_tombstone_edges_are_exactly_the_models_terminal_edges() -> None:
    arrays = _rust_arrays()
    deletable = _sources_of(KNOWLEDGE_TRANSITIONS, KnowledgeStatus.DELETED)
    supersedable = _sources_of(KNOWLEDGE_TRANSITIONS, KnowledgeStatus.SUPERSEDED)
    forgettable = _sources_of(MEMORY_TRANSITIONS, MemoryStatus.DELETED)
    assert arrays["KNOWLEDGE_DELETE_FROM"] == deletable
    assert arrays["KNOWLEDGE_SUPERSEDE_FROM"] == supersedable
    assert arrays["MEMORY_DELETE_FROM"] == forgettable
    # Deletion is terminal on both ladders, so neither table may name `deleted`.
    assert "deleted" not in arrays["KNOWLEDGE_DELETE_FROM"]
    assert "deleted" not in arrays["MEMORY_DELETE_FROM"]


def test_rust_purges_the_same_derived_source_kinds_the_plane_indexes() -> None:
    strings = _rust_strings()
    assert strings["SOURCE_KIND_KNOWLEDGE"] == SOURCE_KIND_KNOWLEDGE
    assert strings["SOURCE_KIND_MEMORY"] == SOURCE_KIND_MEMORY


def test_the_control_applies_no_other_status() -> None:
    # The only statuses the Rust control writes are the two tombstones: a forward edge
    # (candidate → verified → active) is the plane's decision, never a person's button.
    text = RUST_CONTROL.read_text(encoding="utf-8")
    written = set(re.findall(r"SET status = '([a-z_]+)'", text))
    assert written == {"deleted", "superseded"}
