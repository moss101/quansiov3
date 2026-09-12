"""Supply-chain and dependency-security gate tests (OPS-007).

Covers the real repository (the gate is clean) plus one negative fixture per rule:
unpinned dependencies (Rust/Node/Python), a `deny.toml` missing a required section,
a licence outside the allow-list, a known-vulnerable lockfile, SBOM drift, and a
self-promoted (non-quarantined) skill manifest. It also proves the SBOM is
deterministic for a fixed lockfile set.

The vulnerable-dependency fixture is driven by a real published advisory:
`RUSTSEC-2019-0014` (`CVE-2019-16138`, `GHSA-m2pf-hprp-3vqm`, CVSS 9.8 CRITICAL,
patched `>=0.21.3`) against the real crate version `image 0.21.2`, which lies in the
affected range. No network access is required; the advisory corpus is committed under
`tests/ci/fixtures/supply_chain/advisory-db/`.
"""
from __future__ import annotations

import itertools
import json
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Iterable, Set

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from scripts.ci import ci_summary  # noqa: E402
from scripts.ci.supply_chain import advisories, check, lockfiles, policy, sbom, skills  # noqa: E402
from scripts.ci.supply_chain.finding import Finding  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "supply_chain"
ADVISORY_DB = FIXTURES / "advisory-db"


def _rules(findings: Iterable[Finding]) -> Set[str]:
    return {finding.rule for finding in findings}


def _details(findings: Iterable[Finding]) -> str:
    return "\n".join(str(finding) for finding in findings)


# --- the real repository ----------------------------------------------------------
def test_real_repository_passes_the_gate(tmp_path):
    findings, _ = check.run(
        root=ROOT, artifacts_dir=tmp_path, commit="fixture-commit"
    )
    assert findings == [], _details(findings)
    assert (tmp_path / sbom.sbom_filename("fixture-commit")).exists()


def test_check_entry_point_exits_zero_on_the_real_tree(tmp_path):
    result = subprocess.run(
        [
            sys.executable,
            "scripts/ci/supply_chain/check.py",
            "--artifacts-dir",
            str(tmp_path),
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "supply-chain: CLEAN" in result.stdout


def test_real_tree_has_no_false_positive_quarantine_finding():
    assert skills.scan(ROOT) == []


def test_real_tree_has_no_vulnerable_dependency():
    assert advisories.scan(ROOT, ADVISORY_DB) == []


# --- lockfile pinning -------------------------------------------------------------
def test_unpinned_rust_dependency_is_flagged():
    findings = lockfiles.check_rust(FIXTURES / "unpinned")
    assert "unpinned-rust-dependency" in _rules(findings)
    assert "serde" in _details(findings)


def test_unpinned_node_dependency_is_flagged():
    findings = lockfiles.check_node(FIXTURES / "node-unpinned")
    detail = _details(findings)
    assert "unpinned-node-dependency" in _rules(findings)
    assert "left-pad" in detail, "a manifest dependency absent from the lockfile must be reported"
    assert "not frozen" in detail, "a drifted lockfile specifier must be reported"


def test_unpinned_python_dependency_is_flagged():
    findings = lockfiles.check_python(FIXTURES / "python-unpinned")
    assert "unpinned-python-dependency" in _rules(findings)
    assert "pyyaml" in _details(findings)


# --- license / advisory policy ----------------------------------------------------
def test_missing_deny_toml_section_is_flagged():
    findings = policy.check_config(FIXTURES / "deny-missing-section")
    assert "deny-config-incomplete" in _rules(findings)
    assert "[sources]" in _details(findings)


def test_deny_toml_missing_entirely_is_flagged(tmp_path):
    findings = policy.check_config(tmp_path)
    assert "deny-config-incomplete" in _rules(findings)


def test_license_outside_the_allow_list_is_flagged():
    findings = policy.check_config(FIXTURES / "license-unlisted")
    assert "license-not-allowlisted" in _rules(findings)
    assert "GPL-3.0-only" in _details(findings)


def test_cargo_deny_absence_is_informational_not_a_finding(monkeypatch):
    monkeypatch.setattr(policy.shutil, "which", lambda _: None)
    findings, informational = policy.run_cargo_deny(ROOT)
    assert findings == []
    assert [finding.rule for finding in informational] == ["cargo-deny-unavailable"]
    assert informational[0].informational is True


# --- known-vulnerable fixture -----------------------------------------------------
def test_vulnerable_dependency_fixture_is_flagged():
    findings = advisories.scan(
        ROOT, ADVISORY_DB, cargo_lock=FIXTURES / "vulnerable-cargo.lock"
    )
    assert "vulnerable-dependency" in _rules(findings), _details(findings)
    detail = _details(findings)
    assert "image 0.21.2" in detail
    assert "RUSTSEC-2019-0014" in detail
    assert "CVE-2019-16138" in detail
    assert "critical" in detail


def test_advisory_fixture_is_a_real_published_record():
    records, findings = advisories.load_advisories(ADVISORY_DB)
    assert findings == []
    record = next(item for item in records if item.id == "RUSTSEC-2019-0014")
    assert record.package == "image"
    assert record.patched == (">=0.21.3",)
    assert record.unaffected == ("<0.10.2",)
    assert record.aliases == ("CVE-2019-16138", "GHSA-m2pf-hprp-3vqm")
    assert record.matches("0.21.2") is True
    assert record.matches("0.21.3") is False
    assert record.matches("0.10.1") is False, "below the unaffected floor the advisory does not apply"


# --- untrusted skill/tool quarantine ----------------------------------------------
def test_self_promoted_skill_is_flagged():
    findings = skills.scan(FIXTURES / "skills" / "self-promoted")
    assert "skill-self-promoted" in _rules(findings), _details(findings)
    assert "skill-lifecycle-incomplete" in _rules(findings)
    assert "active" in _details(findings)


def test_quarantined_and_approved_skill_passes():
    assert skills.scan(FIXTURES / "skills" / "approved") == []


def test_skill_manifest_without_lifecycle_sections_is_flagged(tmp_path):
    manifest = tmp_path / "skill.json"
    manifest.write_text(json.dumps({"skill_id": "fixture", "status": "draft"}))
    findings = skills.scan(tmp_path)
    assert "skill-lifecycle-incomplete" in _rules(findings)
    assert "skill-self-promoted" not in _rules(findings), "a draft is not a self-promotion"


def test_unparseable_skill_manifest_fails_closed(tmp_path):
    (tmp_path / "skill.json").write_text("{ this is not json")
    findings = skills.scan(tmp_path)
    assert "skill-manifest-unparsable" in _rules(findings)


# --- SBOM -------------------------------------------------------------------------
def test_sbom_is_deterministic(tmp_path):
    first = sbom.generate(ROOT, tmp_path / "first.json", commit="fixture-commit")
    second = sbom.generate(ROOT, tmp_path / "second.json", commit="fixture-commit")
    assert first.read_bytes() == second.read_bytes()


def test_sbom_covers_every_cargo_lockfile_package():
    document = sbom.build(ROOT, "fixture-commit")
    components = {(item["name"], item["version"]) for item in document["components"]}
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    for entry in lock["package"]:
        assert (entry["name"], entry["version"]) in components
    assert document["metadata"]["component"]["name"] == "quansio"
    assert document["metadata"]["component"]["version"] == "fixture-commit"
    assert all(item["purl"].startswith("pkg:") for item in document["components"])
    assert "timestamp" not in document["metadata"], "a timestamp would break determinism"


def test_sbom_drift_is_detected(tmp_path):
    recorded = sbom.generate(ROOT, tmp_path / "sbom.json", commit="fixture-commit")
    assert sbom.verify(ROOT, recorded, commit="fixture-commit") == []
    document = json.loads(recorded.read_text())
    document["components"].append(
        {
            "type": "library",
            "bom-ref": "pkg:cargo/injected@9.9.9",
            "name": "injected",
            "version": "9.9.9",
            "purl": "pkg:cargo/injected@9.9.9",
        }
    )
    recorded.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
    findings = sbom.verify(ROOT, recorded, commit="fixture-commit")
    assert "sbom-drift" in _rules(findings)


def test_sbom_missing_lockfile_is_flagged(tmp_path):
    components, findings = sbom.collect_components(tmp_path)
    assert components == []
    assert "sbom-lockfile-missing" in _rules(findings)


def test_sbom_with_no_components_is_flagged(tmp_path):
    (tmp_path / "Cargo.lock").write_text("version = 3\n")
    (tmp_path / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n\nimporters:\n\n  .: {}\n")
    (tmp_path / "python").mkdir()
    (tmp_path / "python" / "uv.lock").write_text("version = 1\n")
    findings, _ = sbom.check(tmp_path, tmp_path / "artifacts", commit="fixture-commit")
    assert "sbom-lockfile-missing" not in _rules(findings)
    assert "sbom-empty" in _rules(findings)


def test_sbom_nondeterminism_is_flagged(tmp_path, monkeypatch):
    counter = itertools.count()
    real_serialize = sbom.serialize

    def flaky(document):
        return real_serialize(document) + f"# {next(counter)}\n"

    monkeypatch.setattr(sbom, "serialize", flaky)
    findings, _ = sbom.check(ROOT, tmp_path, commit="fixture-commit")
    assert "sbom-nondeterministic" in _rules(findings)


def test_sbom_filename_is_commit_addressed():
    assert sbom.sbom_filename("abcdef1234567890") == "sbom-abcdef123456.json"
    assert sbom.sbom_filename(None) == "sbom-nogit.json"


def test_sbom_verify_mode_fails_on_drift(tmp_path):
    recorded = sbom.generate(ROOT, tmp_path / "sbom.json", commit="fixture-commit")
    assert sbom.main(["--root", str(ROOT), "--verify", str(recorded), "--commit", "fixture-commit"]) == 0
    document = json.loads(recorded.read_text())
    document["components"] = []
    recorded.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
    assert sbom.main(["--root", str(ROOT), "--verify", str(recorded), "--commit", "fixture-commit"]) == 1


def test_check_gate_verifies_a_recorded_sbom(tmp_path):
    recorded = sbom.generate(ROOT, tmp_path / "recorded.json", commit="fixture-commit")
    findings, _ = check.run(
        root=ROOT,
        artifacts_dir=tmp_path / "artifacts",
        commit="fixture-commit",
        sbom_recorded=recorded,
    )
    assert findings == [], _details(findings)
    document = json.loads(recorded.read_text())
    document["metadata"]["component"]["version"] = "different-commit"
    recorded.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
    findings, _ = check.run(
        root=ROOT,
        artifacts_dir=tmp_path / "artifacts",
        commit="fixture-commit",
        sbom_recorded=recorded,
    )
    assert "sbom-drift" in _rules(findings)


# --- pipeline wiring --------------------------------------------------------------
def test_pipeline_gate_list_includes_supply_chain():
    names = [name for name, _ in ci_summary.GATES]
    assert "supply-chain" in names
    assert any(command.endswith("scripts/ci/supply_chain/check.py") for _, command in ci_summary.GATES)
    original = [
        "authority",
        "dossier-consistency",
        "architecture",
        "authority-pointers",
        "workspace",
        "legacy-map",
        "contract-drift",
        "contract-lint-compat",
        "toolchains",
        "tests",
    ]
    assert [name for name in names if name in original] == original


def test_cli_prints_rule_detail_and_exits_nonzero(capsys):
    status = check.main(["--root", str(FIXTURES / "unpinned"), "--only", "lockfiles"])
    captured = capsys.readouterr().out
    assert status == 1
    assert "unpinned-rust-dependency: " in captured
