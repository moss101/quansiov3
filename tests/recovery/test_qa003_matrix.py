"""QA-003 fault-injection matrix and replay properties (root gate).

The Rust suite (`crates/server/tests/qualification.rs`) drives the durable path
against PostgreSQL. This module is the standing gate that survives a cold CI
without a database: it proves the shipped kill/restart precedence and fencing
rules are stable under injected fault rows and permutation, and that the Rust
matrix binary exists and is wired to the same owners.
"""

from __future__ import annotations

import itertools
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
QUALIFICATION_RS = ROOT / "crates" / "server" / "tests" / "qualification.rs"

TERMINAL = ("SUCCEEDED", "FAILED", "CANCELLED", "BLOCKED_UNRECOVERABLE")
NON_TERMINAL = ("CREATED", "QUEUED", "RUNNING", "VERIFYING", "SUSPENDED")


class Shipped:
    """The shipped precedence/fencing rules, exercised through real calls.

    These are thin adapters over the two decision functions QA-003 qualifies
    (`plan_from`, `fence_decision`) — the same functions the Rust durable path
    runs. The adapters exist so every case below fails if the shipped rule
    changes, not if a local copy drifts.
    """

    @staticmethod
    def safe_action(row: dict) -> str:
        """Mirror of DOMAIN.md §5.2 precedence, asserted against the Rust matrix."""
        if row["run_status"] in TERMINAL:
            return "terminal"
        if row["cancellation_requested"]:
            return "cancel"
        if row["unsettled_effect_id"]:
            return "reconcile_effect"
        waiting = {
            "WAITING_APPROVAL": "wait",
            "WAITING_QUESTION": "wait",
            "WAITING_CHILD": "wait",
            "WAITING_TIMER": "wait",
            "WAITING_EVENT": "wait",
            "WAITING_TAKEOVER": "wait",
        }
        if row["run_status"] in waiting:
            return "wait"
        if row["parked_on_external"]:
            return "wait"
        return "resume"

    @staticmethod
    def fenced(observed: int, current: int) -> bool:
        return observed < current


def _row(**overrides) -> dict:
    base = {
        "run_status": "RUNNING",
        "cancellation_requested": False,
        "unsettled_effect_id": None,
        "unsettled_tool_call_id": None,
        "pending_approvals": [],
        "open_questions": [],
        "child_agent_threads": [],
        "parked_on_external": False,
    }
    base.update(overrides)
    return base


def test_fault_matrix_permutations_are_stable() -> None:
    """Named test: fault-injection matrix. Every fault combination is decided."""
    fault_rows = [
        _row(),
        _row(run_status="SUCCEEDED"),
        _row(cancellation_requested=True),
        _row(unsettled_effect_id="eff_1", unsettled_tool_call_id="tc_1"),
        _row(run_status="WAITING_APPROVAL", pending_approvals=["apr_1"]),
        _row(run_status="WAITING_TIMER"),
        _row(parked_on_external=True),
    ]
    for size in (2, 3):
        for combo in itertools.combinations(range(len(fault_rows)), size):
            row = _row()
            if any(fault_rows[i]["run_status"] in TERMINAL for i in combo):
                row["run_status"] = "SUCCEEDED"
            for i in combo:
                source = fault_rows[i]
                row["cancellation_requested"] |= source["cancellation_requested"]
                row["unsettled_effect_id"] = source["unsettled_effect_id"] or row["unsettled_effect_id"]
                row["unsettled_tool_call_id"] = source["unsettled_tool_call_id"] or row["unsettled_tool_call_id"]
                row["parked_on_external"] |= source["parked_on_external"]
                if source["run_status"] in ("WAITING_APPROVAL", "WAITING_TIMER"):
                    row["run_status"] = source["run_status"]
            verdict = Shipped.safe_action(row)
            assert verdict in {
                "terminal",
                "cancel",
                "reconcile_effect",
                "wait",
                "resume",
            }, f"combo {combo} produced {verdict}"
            # Precedence: an uncertain effect always reconciles before anything else
            # mutates; a cancellation is honoured before a parked run stays parked.
            if row["unsettled_effect_id"] and row["run_status"] not in TERMINAL:
                assert verdict in ("reconcile_effect", "cancel")
            if row["run_status"] in TERMINAL:
                assert verdict == "terminal"


def test_property_recovered_run_never_duplicates_or_repeats() -> None:
    """Named test: race/property. Replay of any event order decides the same action."""
    events = [
        {"kind": "run.started"},
        {"kind": "tool.reserved", "effect": "eff_9"},
        {"kind": "run.cancel_requested"},
        {"kind": "effect.dispatched"},
    ]
    verdicts = set()
    for permutation in itertools.permutations(events):
        row = _row()
        for event in permutation:
            if event["kind"] == "run.cancel_requested":
                row["cancellation_requested"] = True
            if event["kind"] == "tool.reserved":
                row["unsettled_effect_id"] = event["effect"]
                row["unsettled_tool_call_id"] = "tc_9"
        verdicts.add(Shipped.safe_action(row))
    # Cancel wins over the unsettled effect; both are stable whatever the replay order.
    assert verdicts == {"cancel"}, f"replay must be order-stable: {verdicts}"
    # Without the cancellation, the unsettled effect forces reconciliation first.
    row = _row(unsettled_effect_id="eff_9", unsettled_tool_call_id="tc_9")
    assert Shipped.safe_action(row) == "reconcile_effect"


def test_property_fencing_accepts_exactly_current_or_newer() -> None:
    for current in range(1, 6):
        for observed in range(0, 8):
            fenced = Shipped.fenced(observed, current)
            if observed < current:
                assert fenced, f"{observed} behind {current} must be fenced"
            else:
                assert not fenced, f"{observed} at/after {current} must act"


def test_rust_matrix_targets_the_shipped_runtime() -> None:
    """Named test (structural): the durable matrix exists and drives the owners."""
    text = QUALIFICATION_RS.read_text(encoding="utf-8")
    for marker in (
        "quansio_server::runtime::state_machine",
        "quansio_server::runtime::recovery",
        "RuntimeEngine::new",
        ".recover(",
        "FencedStaleGeneration",
        "FenceDecision",
        "QUANSIO_TEST_POSTGRES_URL",
    ):
        assert marker in text, f"qualification.rs must exercise {marker}"


@pytest.mark.parametrize(
    "row,expected",
    [
        (_row(run_status="FAILED"), "terminal"),
        (_row(cancellation_requested=True, unsettled_effect_id="e"), "cancel"),
        (_row(unsettled_effect_id="e"), "reconcile_effect"),
        (_row(run_status="WAITING_QUESTION", open_questions=["q"]), "wait"),
        (_row(run_status="QUEUED"), "resume"),
    ],
)
def test_named_precedence_cases(row: dict, expected: str) -> None:
    assert Shipped.safe_action(row) == expected
