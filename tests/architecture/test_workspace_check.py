"""GOV-003 tests: workspace mapping and language-boundary conformance.

Drives the shipped checker against the real repository (must be clean) and against
fixtures that break each rule.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from scripts.ci import workspace_check

ROOT = workspace_check.ROOT


def _crate(root: Path, rel: str, name: str, deps: str = "", owner: str | None = None) -> None:
    d = root / rel
    (d / "src").mkdir(parents=True, exist_ok=True)
    (d / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\n{deps}'
    )
    (d / "src" / "lib.rs").write_text(f'pub const CANONICAL_OWNER: &str = "{owner or rel}";\n')


def _workspace(root: Path, members: list[str]) -> None:
    (root / "Cargo.toml").write_text(
        "[workspace]\nresolver = \"2\"\nmembers = [" + ", ".join(f'"{m}"' for m in members) + "]\n"
    )


def test_real_workspace_is_conformant():
    assert workspace_check.check() == []


def test_owner_map_covers_every_canonical_rust_owner():
    findings = workspace_check.check()
    assert findings == []
    report = workspace_check.render()
    for owner in workspace_check.REQUIRED_RUST_OWNERS:
        assert owner in report


def test_dependency_cycle_is_detected(tmp_path):
    (tmp_path / "pnpm-workspace.yaml").write_text('packages:\n  - "apps/*"\n')
    (tmp_path / "python" / "intelligence").mkdir(parents=True)
    (tmp_path / "python" / "pyproject.toml").write_text(
        '[project]\nrequires-python = ">=3.12"\n[tool.hatch.build.targets.wheel]\npackages = ["intelligence"]\n'
    )
    _workspace(tmp_path, ["crates/core", "crates/graph"])
    _crate(tmp_path, "crates/core", "quansio-core", deps='quansio-graph = { path = "../graph" }')
    _crate(tmp_path, "crates/graph", "quansio-graph", deps='quansio-core = { path = "../core" }')
    findings = workspace_check.check(tmp_path, require_all_owners=False)
    rules = [f["rule"] for f in findings]
    assert "dependency-cycle" in rules


def test_owner_constant_mismatch_is_detected(tmp_path):
    (tmp_path / "pnpm-workspace.yaml").write_text('packages:\n  - "apps/*"\n')
    (tmp_path / "python").mkdir()
    (tmp_path / "python" / "pyproject.toml").write_text(
        '[project]\nrequires-python = ">=3.12"\n[tool.hatch.build.targets.wheel]\npackages = ["intelligence"]\n'
    )
    _workspace(tmp_path, ["crates/core"])
    _crate(tmp_path, "crates/core", "quansio-core", owner="crates/not-core")
    rules = [f["rule"] for f in workspace_check.check(tmp_path, require_all_owners=False)]
    assert "wrong-owner-constant" in rules


def test_serde_dependency_is_not_treated_as_workspace_edge(tmp_path):
    (tmp_path / "pnpm-workspace.yaml").write_text('packages:\n  - "apps/*"\n')
    (tmp_path / "python" / "intelligence").mkdir(parents=True)
    (tmp_path / "python" / "pyproject.toml").write_text(
        '[project]\nrequires-python = ">=3.12"\n[tool.hatch.build.targets.wheel]\npackages = ["intelligence"]\n'
    )
    _workspace(tmp_path, ["crates/core"])
    _crate(tmp_path, "crates/core", "quansio-core", deps='serde = "1"')
    assert workspace_check.check(tmp_path, require_all_owners=False) == []


def test_missing_pnpm_and_python_workspaces_are_reported(tmp_path):
    _workspace(tmp_path, [])
    rules = {f["rule"] for f in workspace_check.check(tmp_path, require_all_owners=False)}
    assert {"empty-cargo-workspace", "missing-pnpm-workspace", "missing-python-workspace"} <= rules


def test_pnpm_package_outside_boundary_is_reported(tmp_path):
    (tmp_path / "pnpm-workspace.yaml").write_text('packages:\n  - "tools/*"\n')
    (tmp_path / "tools" / "helper").mkdir(parents=True)
    (tmp_path / "tools" / "helper" / "package.json").write_text('{"name":"helper"}')
    (tmp_path / "python" / "intelligence").mkdir(parents=True)
    (tmp_path / "python" / "pyproject.toml").write_text(
        '[project]\nrequires-python = ">=3.12"\n[tool.hatch.build.targets.wheel]\npackages = ["intelligence"]\n'
    )
    _workspace(tmp_path, [])
    rules = {f["rule"] for f in workspace_check.check(tmp_path, require_all_owners=False)}
    assert "node-package-outside-boundary" in rules


def test_missing_required_owner_package_is_reported(tmp_path):
    (tmp_path / "pnpm-workspace.yaml").write_text('packages:\n  - "apps/*"\n')
    (tmp_path / "python" / "intelligence").mkdir(parents=True)
    (tmp_path / "python" / "pyproject.toml").write_text(
        '[project]\nrequires-python = ">=3.12"\n[tool.hatch.build.targets.wheel]\npackages = ["intelligence"]\n'
    )
    _workspace(tmp_path, ["crates/core"])
    _crate(tmp_path, "crates/core", "quansio-core")
    rules = {f["rule"] for f in workspace_check.check(tmp_path)}
    assert "missing-owner-package" in rules


def test_cli_reports_clean_and_findings(tmp_path, capsys):
    assert workspace_check.main(["--json"]) == 0
    assert capsys.readouterr().out.strip() == "[]"
    (tmp_path / "Cargo.toml").write_text('[workspace]\nmembers = []\n')
    assert workspace_check.main(["--root", str(tmp_path)]) == 1


def test_every_crate_readme_names_its_owner():
    for member in workspace_check.REQUIRED_RUST_OWNERS:
        readme = ROOT / member / "README.md"
        assert readme.exists(), f"{member} must have a README.md naming its canonical owner role"
        text = readme.read_text()
        assert len(text.splitlines()) <= 20, f"{member} README must stay under 20 lines"
        assert member in text


@pytest.mark.parametrize("path", ["crates/server/README.md", "python/intelligence/README.md"])
def test_owner_readmes_exist(path):
    assert (ROOT / path).exists()
