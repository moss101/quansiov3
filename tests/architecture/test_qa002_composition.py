"""QA-002: every authority the build ships is composed into it, or is a named, owned gap.

`composition_scan` finds ports whose real implementation production never constructs, ports that
run only on their fail-closed default, and named authorities production never constructs or calls.
The tree's findings must equal `KNOWN_GAPS` exactly: a new gap fails (an authority was built and
left out of the build), and so does a closed one still listed (the record must follow the code).
The negative corpus proves each rule on synthetic source trees.
"""

from __future__ import annotations

import json
import sys
import textwrap
from pathlib import Path
from typing import Dict, Tuple

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import composition_scan as scan  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

#: (kind, name) -> (owning task, what is missing). Found by QA-001/QA-002, 2026-09-27.
KNOWN_GAPS: Dict[Tuple[str, str], Tuple[str, str]] = {
    ("DEFAULT_ONLY", "MemoryProposalPort"): ("INT-007", "memory.propose reaches only the unavailable port"),
    ("NOT_COMPOSED", "WorkGraphPort"): ("RUN-004", "GraphWorkGraph is never constructed"),
    ("UNCOMPOSED", "GraphWorkGraph"): ("RUN-004", "as above"),
    ("UNCOMPOSED", "Orchestrator"): ("RUN-004", "fan-out/fan-in, the ready queue and subtree cancellation never run"),
    ("UNCOMPOSED", "OutboxPublisher"): ("CORE-003", "no event transport; the transactional outbox is never published"),
    ("UNCOMPOSED", "KmsKeyProvider"): ("EXEC-007", "only the local master key provider is composed"),
    ("UNCOMPOSED", "ComputerDispatcher"): ("EXEC-010", "computer use has no composed dispatcher"),
    ("UNCOMPOSED", "WorkerHost"): ("EXEC-002", "qworkerd has no daemon that runs its host"),
}


def findings() -> Dict[Tuple[str, str], str]:
    found = scan.scan(ROOT) + scan.uncomposed(ROOT)
    return {finding.key(): finding.detail for finding in found}


def test_every_authority_is_composed_or_a_named_owned_gap() -> None:
    actual = findings()
    new = {key: detail for key, detail in actual.items() if key not in KNOWN_GAPS}
    closed = sorted(key for key in KNOWN_GAPS if key not in actual)
    assert not new, f"authorities built but left out of the build: {json.dumps(new, indent=2)}"
    assert not closed, f"gaps now composed; remove them from KNOWN_GAPS: {closed}"


def test_every_known_gap_names_a_real_owning_task_and_every_obligation_is_defined() -> None:
    tasks = {task["id"] for task in json.loads((ROOT / "registries" / "tasks.json").read_text())["tasks"]}
    assert {owner for owner, _ in KNOWN_GAPS.values()} <= tasks
    assert {obligation.owner for obligation in scan.OBLIGATIONS} <= tasks
    undefined = [f for f in scan.uncomposed(ROOT) if f.kind == "UNDEFINED"]
    assert undefined == [], "an obligation names something the tree no longer defines (renamed?)"


def test_the_runtime_lifecycle_the_build_runs_is_composed() -> None:
    """The composed authorities stay composed (the ones this campaign wired, among others)."""
    actual = findings()
    for name in ("recover_on_start", "expire_due", "RuntimeBudgets", "ToolDispatchService",
                 "Scheduler", "FireExecutor", "catch_up", "TenantMemberRoles", "RuntimeEngine",
                 "ContractVerifier", "GatewaySemanticVerifier"):
        assert ("UNCOMPOSED", name) not in actual, name


# ------------------------------------------------------------------------------ negative corpus


def tree(tmp_path: Path, files: Dict[str, str]) -> Path:
    for relative, body in files.items():
        path = tmp_path / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(body))
    return tmp_path


PORT = """
    pub trait LedgerPort: Send + Sync { fn record(&self); }
    pub struct UnavailableLedger;
    impl LedgerPort for UnavailableLedger { fn record(&self) {} }
    pub struct PostgresLedger;
    impl LedgerPort for PostgresLedger { fn record(&self) {} }
"""


def kinds(root: Path) -> Dict[str, str]:
    return {finding.trait: finding.kind for finding in scan.scan(root)}


def test_a_real_port_constructed_only_by_tests_is_not_composed(tmp_path: Path) -> None:
    root = tree(tmp_path, {
        "crates/app/src/port.rs": PORT,
        "crates/app/src/engine.rs": """
            use crate::port::PostgresLedger; // a use is not a construction
            pub fn engine() -> Box<dyn LedgerPort> { Box::new(UnavailableLedger) }
            /// Doc: PostgresLedger is what production should run.
            #[cfg(test)]
            mod tests {
                fn real() -> PostgresLedger { PostgresLedger }
            }
        """,
    })
    assert kinds(root) == {"LedgerPort": "NOT_COMPOSED"}


def test_a_port_that_runs_only_on_its_default_is_caught(tmp_path: Path) -> None:
    root = tree(tmp_path, {
        "crates/app/src/port.rs": """
            pub trait LedgerPort: Send + Sync { fn record(&self); }
            pub struct UnavailableLedger;
            impl LedgerPort for UnavailableLedger { fn record(&self) {} }
        """,
        "crates/app/src/engine.rs": "pub fn engine() -> Box<dyn LedgerPort> { Box::new(UnavailableLedger) }\n",
    })
    assert kinds(root) == {"LedgerPort": "DEFAULT_ONLY"}


def test_a_composed_real_port_is_clean_and_foreign_traits_are_ignored(tmp_path: Path) -> None:
    root = tree(tmp_path, {
        "crates/app/src/port.rs": PORT + "impl Clone for PostgresLedger { fn clone(&self) -> Self { Self } }\n",
        "crates/app/src/engine.rs": "pub fn engine() -> Box<dyn LedgerPort> { Box::new(PostgresLedger) }\n",
        "crates/app/src/generated/api.rs": "pub trait Api {}\npub struct ApiServer;\nimpl Api for ApiServer {}\n",
    })
    assert kinds(root) == {}


def uncomposed_names(root: Path, *obligations: scan.Obligation) -> Dict[str, str]:
    return {f.trait: f.kind for f in scan.uncomposed(root, obligations)}


SERVICE = scan.Obligation("Recoverer", "type", "RUN-009", "recovery")
ENTRY = scan.Obligation("expire_due", "call", "RUN-011", "question expiry")


def test_a_service_that_only_builds_itself_is_uncomposed(tmp_path: Path) -> None:
    root = tree(tmp_path, {
        "crates/app/src/recovery.rs": """
            pub struct Recoverer;
            impl Recoverer { pub fn new() -> Self { Self } pub fn expire_due(&self) {} }
            /// Recoverer::new() in a comment is not a construction.
            #[cfg(test)]
            mod tests { fn build() { let r = Recoverer::new(); r.expire_due(); } }
        """,
    })
    assert uncomposed_names(root, SERVICE, ENTRY) == {"Recoverer": "UNCOMPOSED", "expire_due": "UNCOMPOSED"}


def test_a_service_built_by_the_composition_root_is_composed(tmp_path: Path) -> None:
    root = tree(tmp_path, {
        "crates/app/src/recovery.rs": "pub struct Recoverer;\nimpl Recoverer { pub fn new() -> Self { Self } pub fn expire_due(&self) {} }\n",
        "crates/app/src/composition.rs": "pub fn start() { Recoverer::new().expire_due(); }\n",
    })
    assert uncomposed_names(root, SERVICE, ENTRY) == {}


def test_a_composition_root_may_define_and_run_its_own_entry_point(tmp_path: Path) -> None:
    root = tree(tmp_path, {
        "crates/app/src/composition.rs": """
            pub fn recover_on_start() {}
            pub fn serve() { recover_on_start(); }
        """,
    })
    assert uncomposed_names(root, scan.Obligation("recover_on_start", "call", "RUN-009", "recovery")) == {}


def test_a_renamed_obligation_is_reported_rather_than_passing(tmp_path: Path) -> None:
    root = tree(tmp_path, {"crates/app/src/lib.rs": "pub struct Something;\n"})
    assert uncomposed_names(root, SERVICE) == {"Recoverer": "UNDEFINED"}


@pytest.mark.parametrize(
    "source",
    [
        'pub fn f() { let s = "impl LedgerPort for PostgresLedger"; }',  # inside a string literal
        "/* PostgresLedger::new() */ pub fn f() {}",  # inside a block comment
    ],
)
def test_strings_and_comments_never_count_as_construction(tmp_path: Path, source: str) -> None:
    root = tree(tmp_path, {"crates/app/src/port.rs": PORT, "crates/app/src/engine.rs": source + "\n"})
    assert kinds(root) == {"LedgerPort": "NOT_COMPOSED"}
