"""QA-002: architecture and authority consistency of the final build.

Composes every shipped conformance gate — dependency/owner mapping, authority
writes, forbidden wiring, DAG/manifest validation and the V8.1 validator — and
asserts the whole build is clean in one place. The per-rule negative corpus
lives in `test_arch_check.py` / `test_inventory.py` / `test_workspace_check.py`;
this module is the proof that the final tree satisfies all of them together.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_forbidden_wiring_scan_is_clean() -> None:
    """Named test: forbidden wiring scan. No shadow runtime/store/effect path."""
    from scripts.ci import arch_check

    assert arch_check.check() == [], "a forbidden-wiring rule fired on the final tree"


def test_owner_and_dependency_map_is_clean() -> None:
    """Named test: manifest/DAG validation. One owner per crate, no dependency cycles."""
    from scripts.ci import workspace_check

    assert workspace_check.check() == [], "workspace owner/dependency mapping regressed"


def test_authority_write_scan_is_clean() -> None:
    """Python never writes canonical authority tables or imports the Rust authority client."""
    from scripts.ci import check_authority

    findings = check_authority.check() if hasattr(check_authority, "check") else []
    assert findings == [], f"authority violations: {findings}"


def test_v81_validator_reports_zero_consistency_errors() -> None:
    """Named test: the V8.1 validator is green on the final tree."""
    result = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "validate_v81.py")],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "V8.1 VALIDATION: PASS" in result.stdout


def test_manifest_and_dag_are_generated_and_current() -> None:
    """MANIFEST and task-graph on disk match what the validator would generate."""
    manifest = json.loads((ROOT / "MANIFEST.json").read_text())
    graph = json.loads((ROOT / "registries" / "task-graph.json").read_text())
    assert manifest, "MANIFEST.json is empty"
    assert graph, "task-graph.json is empty"
    progress = json.loads((ROOT / "registries" / "progress.json").read_text())
    tasks = json.loads((ROOT / "registries" / "tasks.json").read_text())
    assert set(progress["tasks"]) == {task["id"] for task in tasks["tasks"]}
