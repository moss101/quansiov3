"""GOV-002 tests: authority pointers and archive exclusion.

Drives the shipped checker against the real repository (must be clean) and against
fixture trees that break each rule (must be detected).
"""
from __future__ import annotations

from pathlib import Path

from scripts.ci import check_authority

ROOT = check_authority.ROOT

BANNERED = "# NON-AUTHORITY — archived\n\nsuperseded\n"


def _repo(tmp_path: Path) -> Path:
    (tmp_path / "docs" / "archive").mkdir(parents=True)
    (tmp_path / "README.md").write_text(
        "See DOSSIER.md, DOMAIN.md, AGENTS.md, registries/tasks.json, registries/progress.json.\n"
    )
    (tmp_path / "docs" / "archive" / "README.md").write_text(BANNERED)
    return tmp_path


def test_real_repository_authority_check_is_clean():
    assert check_authority.check() == []


def test_missing_authority_reference_is_reported(tmp_path):
    root = _repo(tmp_path)
    (root / "README.md").write_text("Quansio. See DOSSIER.md only.\n")
    rules = [f["rule"] for f in check_authority.check(root)]
    assert "missing-authority-pointer" in rules


def test_archive_file_without_banner_is_reported(tmp_path):
    root = _repo(tmp_path)
    (root / "docs" / "archive" / "r1-dossier.md").write_text("# old dossier\n")
    findings = check_authority.check(root)
    assert [f["rule"] for f in findings] == ["archive-file-without-banner"]


def test_active_instruction_pointing_at_unbannered_archive_is_reported(tmp_path):
    root = _repo(tmp_path)
    (root / "docs" / "archive" / "r1-dossier.md").write_text("# old dossier\n")
    (root / "AGENTS.md").write_text("Read docs/archive/r1-dossier.md for the architecture.\n")
    rules = [f["rule"] for f in check_authority.check(root)]
    assert "archive-reference-without-banner" in rules


def test_bannered_archive_reference_is_allowed(tmp_path):
    root = _repo(tmp_path)
    (root / "docs" / "archive" / "r1-dossier.md").write_text(BANNERED)
    (root / "AGENTS.md").write_text("Historical input only: docs/archive/r1-dossier.md\n")
    assert check_authority.check(root) == []


def test_cli_exit_codes(tmp_path):
    root = _repo(tmp_path)
    assert check_authority.main(["--check", "--root", str(root)]) == 0
    (root / "docs" / "archive" / "bad.md").write_text("no banner here\n")
    assert check_authority.main(["--check", "--root", str(root)]) == 1
    assert check_authority.main(["--check", "--json", "--root", str(root)]) == 1


def test_archive_readme_declares_non_authority():
    readme = (ROOT / "docs" / "archive" / "README.md").read_text()
    assert "NON-AUTHORITY" in readme
    assert "not" in readme.lower() and "authority" in readme.lower()


def test_readme_names_authority_set_and_never_superseded_material():
    readme = (ROOT / "README.md").read_text()
    for ref in check_authority.REQUIRED_README_REFERENCES:
        assert ref in readme
    # No generated view may be presented as hand-editable authority.
    assert "never hand-edited" in readme or "never hand edited" in readme
