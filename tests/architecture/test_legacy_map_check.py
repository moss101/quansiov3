"""GOV-006 tests: legacy migration-map completeness and direct-path scan.

Drives the shipped checker against the real repository (clean, greenfield) and against
fixtures where legacy code appears without a disposition, or where a DELETE row still
resolves to an existing path.
"""
from __future__ import annotations

from pathlib import Path

from scripts.ci import legacy_map_check

ROOT = legacy_map_check.ROOT

MAP_HEADER = "# Legacy migration and deletion map\n\n| Artifact | Disposition | Canonical owner or replacement |\n|---|---|---|\n"


def _fixture(tmp_path: Path, rows: str = "") -> Path:
    (tmp_path / "docs" / "review").mkdir(parents=True)
    (tmp_path / legacy_map_check.MAP_PATH).write_text(MAP_HEADER + rows)
    return tmp_path


def test_real_repository_has_no_legacy_authority():
    assert legacy_map_check.check() == []
    assert legacy_map_check.legacy_artifacts(ROOT) == []


def test_migration_map_exists_and_is_declared_greenfield():
    text = (ROOT / legacy_map_check.MAP_PATH).read_text()
    assert "no pre-existing product code" in text
    assert "KEEP_BEHIND_BOUNDARY" in text and "DELETE" in text
    assert legacy_map_check.parse_map(ROOT) == []


def test_legacy_artifact_without_disposition_is_reported(tmp_path):
    root = _fixture(tmp_path)
    (root / "legacy").mkdir()
    (root / "legacy" / "orchestrator.py").write_text("x = 1\n")
    findings = legacy_map_check.check(root)
    rules = [f["rule"] for f in findings]
    assert "legacy-without-disposition" in rules
    # A legacy directory is also not a canonical owner, which the direct-path scan reports.
    assert "legacy-direct-path" in rules


def test_go_file_without_disposition_is_reported(tmp_path):
    root = _fixture(tmp_path)
    (root / "service").mkdir()
    (root / "service" / "main.go").write_text("package main\n")
    findings = legacy_map_check.check(root)
    rules = [f["rule"] for f in findings]
    assert "legacy-without-disposition" in rules
    assert "legacy-direct-path" in rules


def test_delete_disposition_for_existing_path_is_reported(tmp_path):
    root = _fixture(tmp_path, "| `legacy/orchestrator.py` | DELETE | `crates/server/src/runtime` |\n")
    (root / "legacy").mkdir()
    (root / "legacy" / "orchestrator.py").write_text("x = 1\n")
    findings = legacy_map_check.check(root)
    assert "disposition-path-still-present" in [f["rule"] for f in findings]


def test_delete_disposition_for_absent_path_is_clean(tmp_path):
    root = _fixture(tmp_path, "| `legacy/orchestrator.py` | DELETE | `crates/server/src/runtime` |\n")
    assert legacy_map_check.check(root) == []


def test_keep_row_needs_existing_path_and_known_owner(tmp_path):
    root = _fixture(
        tmp_path,
        "| `crates/core/legacy_parser.py` | KEEP_BEHIND_BOUNDARY | `crates/core` |\n",
    )
    findings = legacy_map_check.check(root)
    assert [f["rule"] for f in findings] == ["kept-artifact-missing"]

    (root / "crates" / "core").mkdir(parents=True)
    (root / "crates" / "core" / "legacy_parser.py").write_text("x = 1\n")
    assert legacy_map_check.check(root) == []


def test_keep_row_with_unknown_owner_is_reported(tmp_path):
    root = _fixture(tmp_path, "| `crates/core/legacy_parser.py` | PORT | `somewhere/else` |\n")
    (root / "crates" / "core").mkdir(parents=True)
    (root / "crates" / "core" / "legacy_parser.py").write_text("x = 1\n")
    rules = [f["rule"] for f in legacy_map_check.check(root)]
    assert "kept-artifact-owner-unknown" in rules


def test_missing_map_is_reported(tmp_path):
    findings = legacy_map_check.check(tmp_path)
    assert [f["rule"] for f in findings] == ["missing-migration-map"]


def test_direct_path_scan_is_wired_to_the_duplicate_authority_scan(tmp_path):
    root = _fixture(tmp_path)
    rogue = root / "python" / "intelligence" / "context" / "rogue.py"
    rogue.parent.mkdir(parents=True)
    rogue.write_text("import anthropic\n")
    findings = legacy_map_check.check(root)
    assert "legacy-direct-path" in [f["rule"] for f in findings]


def test_cli_exit_codes(tmp_path, capsys):
    assert legacy_map_check.main(["--json"]) == 0
    assert capsys.readouterr().out.strip() == "[]"
    assert legacy_map_check.main(["--root", str(tmp_path)]) == 1
