"""GOV-001 tests: repository inventory and duplicate-authority scan.

The tests drive the shipped scanner (`scripts/ci/inventory.py`) against the real
repository and against fixture trees; they fail if the scanner stops detecting
competing-authority code.
"""
from __future__ import annotations

import json
from pathlib import Path

from scripts.ci import inventory

ROOT = inventory.ROOT


def test_inventory_covers_authority_set_and_real_repository():
    inv = inventory.inventory()
    assert inv["git"]["is_repository"] is True
    assert inv["authority_files_missing"] == []
    assert inv["authority_files"] == inventory.AUTHORITY_FILES
    # Every product-code file must map to a canonical owner.
    assert inv["files_without_canonical_owner"] == []


def test_real_repository_scan_is_clean_greenfield():
    # Greenfield: only the authority set and tooling exist, so no competing
    # authority path may be reported.
    findings = inventory.scan()
    assert findings == [], f"unexpected duplicate-authority findings: {findings}"


def test_inventory_cli_json_is_machine_readable(tmp_path, capsys):
    rc = inventory.main(["--json"])
    assert rc == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["git"]["is_repository"] is True
    assert "product_code_by_language" in payload


def _scan_fixture(tmp_path: Path, rel: str, content: str):
    target = tmp_path / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content)
    return inventory.scan(files=[rel], root=tmp_path)


def test_scan_detects_non_rust_authority_write(tmp_path):
    findings = _scan_fixture(
        tmp_path,
        "python/intelligence/context/rogue.py",
        "def commit(conn):\n    conn.execute('UPDATE effect_ledger SET settled = true')\n",
    )
    assert [f["rule"] for f in findings] == ["non-rust-authority-write"]


def test_scan_detects_provider_sdk_outside_gateway(tmp_path):
    findings = _scan_fixture(
        tmp_path,
        "python/intelligence/context/rogue.py",
        "import anthropic\n\nclient = anthropic.Anthropic(api_key='x')\n",
    )
    assert [f["rule"] for f in findings] == ["provider-sdk-outside-gateway"]


def test_scan_allows_provider_sdk_inside_gateway(tmp_path):
    findings = _scan_fixture(
        tmp_path,
        "python/intelligence/model_gateway/anthropic_adapter.py",
        "import anthropic\n",
    )
    assert findings == []


def test_scan_detects_client_database_access(tmp_path):
    findings = _scan_fixture(
        tmp_path,
        "apps/desktop/renderer/src/store.ts",
        "import { Pool } from 'pg';\nconst pool = new Pool();\n",
    )
    assert [f["rule"] for f in findings] == ["client-direct-database"]


def test_scan_detects_tool_registration_outside_rust(tmp_path):
    findings = _scan_fixture(
        tmp_path,
        "apps/web/src/tools.ts",
        "registerTool('shell.exec', handler);\n",
    )
    assert [f["rule"] for f in findings] == ["tool-registry-outside-rust"]


def test_scan_detects_hardcoded_model_id_but_not_in_config(tmp_path):
    findings = _scan_fixture(
        tmp_path,
        "python/intelligence/model_gateway/route.py",
        "DEFAULT = 'claude-sonnet-4-5'\n",
    )
    assert [f["rule"] for f in findings] == ["hardcoded-model-id"]

    in_config = _scan_fixture(
        tmp_path,
        "config/models.yaml",
        "models:\n  - id: claude-sonnet-4-5\n",
    )
    assert in_config == []


def test_scan_detects_new_go_code(tmp_path):
    findings = _scan_fixture(tmp_path, "crates/core/helper.go", "package main\n")
    assert "new-go-code" in [f["rule"] for f in findings]


def test_scan_detects_file_without_canonical_owner(tmp_path):
    findings = _scan_fixture(tmp_path, "mystery/thing.py", "x = 1\n")
    assert [f["rule"] for f in findings] == ["no-canonical-owner"]


def test_every_canonical_owner_has_a_declared_path():
    reg_owners = {t["component"] for t in json.loads((ROOT / "registries/tasks.json").read_text())["tasks"]}
    assert reg_owners, "task registry must declare components"
    # Path prefixes must be repository-relative and non-empty.
    for owner, prefixes in inventory.CANONICAL_OWNERS.items():
        assert prefixes, owner
        for prefix in prefixes:
            assert not prefix.startswith("/") and prefix.endswith("/"), f"{owner}: {prefix}"


def test_reconciliation_report_and_evidence_committed():
    report = ROOT / "docs" / "review" / "2026-09-12-gov-001-reconciliation.md"
    assert report.exists(), "GOV-001 reconciliation report must be committed"
    text = report.read_text()
    for needle in ("GENUINE_GAP", "no legacy authority found", "duplicate-authority", "canonical owner"):
        assert needle in text, f"reconciliation report must state: {needle}"
    bundles = sorted((ROOT / "evidence" / "GOV-001").glob("*/summary.json"))
    assert bundles, "GOV-001 evidence bundle must be committed"
    summary = json.loads(bundles[-1].read_text())
    assert summary["task_id"] == "GOV-001"
    assert summary["commands"] and summary["results"]
