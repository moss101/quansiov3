"""GOV-005 tests: CI baseline pipeline, validator negative cases and dossier consistency.

The negative cases tamper with a copy of the authority set and run the *shipped*
`scripts/validate_v81.py` as a subprocess, exactly as CI does, so a regression in the
validator's failure behaviour is caught here.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

from scripts.ci import ci_summary, dossier_consistency

ROOT = ci_summary.ROOT

AUTHORITY_FILES = [
    "DOSSIER.md",
    "DOMAIN.md",
    "AGENTS.md",
    "IMPLEMENTATION_MASTER_PROMPT.md",
    "TASKS.md",
    "MANIFEST.json",
    "registries/tasks.json",
    "registries/task-graph.json",
    "registries/progress.json",
]


def authority_copy(tmp_path: Path) -> Path:
    """A disposable copy of the authority set plus the validator script."""
    root = tmp_path / "repo"
    (root / "scripts").mkdir(parents=True)
    (root / "registries").mkdir(parents=True)
    for relative in AUTHORITY_FILES:
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / relative, target)
    shutil.copy2(ROOT / "scripts" / "validate_v81.py", root / "scripts" / "validate_v81.py")
    # Normalise the generated views so a tampering test reports only the tampering.
    subprocess.run(
        [sys.executable, str(root / "scripts" / "validate_v81.py"), "--write"],
        cwd=str(root),
        check=True,
        capture_output=True,
    )
    return root


def run_validator(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(root / "scripts" / "validate_v81.py")],
        cwd=str(root),
        capture_output=True,
        text=True,
    )


def test_untampered_copy_passes(tmp_path):
    result = run_validator(authority_copy(tmp_path))
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout.startswith("V8.1 VALIDATION: PASS")


def test_negative_manifest_tamper_fails(tmp_path):
    root = authority_copy(tmp_path)
    manifest = json.loads((root / "MANIFEST.json").read_text())
    manifest["files"][0]["sha256"] = "0" * 64
    (root / "MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n")
    result = run_validator(root)
    assert result.returncode == 1
    assert "MANIFEST.json is stale" in result.stdout


def test_negative_dependency_cycle_fails(tmp_path):
    root = authority_copy(tmp_path)
    tasks = json.loads((root / "registries" / "tasks.json").read_text())
    by_id = {task["id"]: task for task in tasks["tasks"]}
    by_id["GOV-001"]["depends_on"] = ["GOV-002"]
    (root / "registries" / "tasks.json").write_text(json.dumps(tasks, indent=2) + "\n")
    result = run_validator(root)
    assert result.returncode == 1
    assert "cycle" in result.stdout


def test_negative_unknown_dependency_fails(tmp_path):
    root = authority_copy(tmp_path)
    tasks = json.loads((root / "registries" / "tasks.json").read_text())
    tasks["tasks"][1]["depends_on"] = ["NOPE-999"]
    (root / "registries" / "tasks.json").write_text(json.dumps(tasks, indent=2) + "\n")
    result = run_validator(root)
    assert result.returncode == 1
    assert "unknown dependency NOPE-999" in result.stdout


def test_negative_generated_view_drift_fails(tmp_path):
    root = authority_copy(tmp_path)
    with (root / "TASKS.md").open("a") as handle:
        handle.write("\n<!-- hand edit -->\n")
    result = run_validator(root)
    assert result.returncode == 1
    assert "TASKS.md is stale" in result.stdout


def test_negative_task_scope_shrink_fails(tmp_path):
    """Removing an acceptance statement from a task is a scope change the validator sees."""
    root = authority_copy(tmp_path)
    tasks = json.loads((root / "registries" / "tasks.json").read_text())
    tasks["tasks"][0]["acceptance"] = []
    (root / "registries" / "tasks.json").write_text(json.dumps(tasks, indent=2) + "\n")
    result = run_validator(root)
    assert result.returncode == 1
    assert "acceptance must be a non-empty list of strings" in result.stdout


def test_negative_fabricated_pass_is_rejected(tmp_path):
    """A PASS with no commit, tests, artifacts or evidence cannot validate."""
    root = authority_copy(tmp_path)
    progress = json.loads((root / "registries" / "progress.json").read_text())
    progress["tasks"]["GOV-001"].update(
        {
            "status": "PASS",
            "claimed_by": "agent:x",
            "started_at": "2026-09-12T00:00:00Z",
            "updated_at": "2026-09-12T00:00:00Z",
            "coverage": "GENUINE_GAP",
            "git_commit": None,
            "tests": [],
            "artifacts": [],
            "implementation_complete": True,
        }
    )
    (root / "registries" / "progress.json").write_text(json.dumps(progress, indent=2) + "\n")
    result = run_validator(root)
    assert result.returncode == 1
    assert "PASS missing git_commit" in result.stdout


def test_real_repository_is_dossier_consistent():
    assert dossier_consistency.check() == []


def test_dossier_consistency_detects_unreachable_commit(tmp_path):
    root = tmp_path / "repo"
    (root / "registries").mkdir(parents=True)
    (root / "evidence").mkdir(parents=True)
    (root / "registries" / "progress.json").write_text(
        json.dumps(
            {
                "tasks": {
                    "GOV-001": {
                        "status": "PASS",
                        "git_commit": "deadbee",
                        "tests": ["pytest"],
                        "artifacts": [],
                    }
                }
            }
        )
    )
    findings = dossier_consistency.check(root, run_validator=False)
    rules = {finding["rule"] for finding in findings}
    assert "unreachable-commit" in rules
    assert "missing-evidence" in rules


def test_dossier_consistency_detects_missing_artifact_and_tests(tmp_path):
    root = tmp_path / "repo"
    (root / "registries").mkdir(parents=True)
    (root / "evidence" / "GOV-001" / "2026-09-12T00-00-00Z").mkdir(parents=True)
    (root / "evidence" / "GOV-001" / "2026-09-12T00-00-00Z" / "summary.json").write_text(
        json.dumps({"task_id": "GOV-001"})
    )
    (root / "registries" / "progress.json").write_text(
        json.dumps(
            {
                "tasks": {
                    "GOV-001": {
                        "status": "PASS",
                        "git_commit": None,
                        "tests": [],
                        "artifacts": ["evidence/GOV-001/nope.json"],
                    }
                }
            }
        )
    )
    findings = dossier_consistency.check(root, run_validator=False)
    rules = {finding["rule"] for finding in findings}
    assert {"pass-without-commit", "pass-without-tests", "missing-artifact", "invalid-evidence"} <= rules


def test_commit_reachability_uses_git(tmp_path):
    repo = tmp_path / "git-repo"
    repo.mkdir()
    env = {
        "GIT_AUTHOR_NAME": "t",
        "GIT_AUTHOR_EMAIL": "t@example.com",
        "GIT_COMMITTER_NAME": "t",
        "GIT_COMMITTER_EMAIL": "t@example.com",
        "PATH": "/usr/bin:/bin:/usr/local/bin",
        "HOME": str(tmp_path),
    }
    subprocess.run(["git", "init", "-b", "main"], cwd=repo, check=True, capture_output=True, env=env)
    (repo / "file.txt").write_text("one")
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True, env=env)
    subprocess.run(["git", "commit", "-m", "one"], cwd=repo, check=True, capture_output=True, env=env)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, check=True, capture_output=True, text=True, env=env
    ).stdout.strip()
    assert dossier_consistency.commit_reachable(head, repo)
    assert not dossier_consistency.commit_reachable("deadbeef", repo)


def test_ci_summary_records_status_and_writes_machine_readable_summary(tmp_path):
    out = tmp_path / "summary.json"
    summary = ci_summary.run(["exit 0", "exit 3"], out=out, root=tmp_path)
    assert summary["status"] == "FAIL"
    assert [result["status"] for result in summary["results"]] == ["PASS", "FAIL"]
    written = json.loads(out.read_text())
    assert written["status"] == "FAIL"
    assert written["results"][1]["exit_code"] == 3
    assert "commit" in written and "recorded_at" in written


def test_ci_summary_exit_code_follows_the_pipeline(tmp_path):
    assert ci_summary.main(["--command", "exit 0", "--out", str(tmp_path / "ok.json")]) == 0
    assert ci_summary.main(["--command", "exit 1", "--out", str(tmp_path / "bad.json")]) == 1


def test_optional_gate_is_never_silently_passed(tmp_path):
    """A gate whose entry point does not exist yet must be reported, not assumed green."""
    summary = ci_summary.run([], out=tmp_path / "s.json", root=tmp_path)
    architecture = next(r for r in summary["results"] if r["gate"] == "architecture")
    assert architecture["status"] == "SKIPPED_PENDING"
    assert architecture["status"] != "PASS"


def test_ci_gate_list_covers_every_required_pipeline_stage():
    commands = " ".join(command for _, command in ci_summary.GATES)
    for needle in (
        "validate_v81.py",
        "dossier_consistency.py",
        "arch_check.py",
        "check_authority.py",
        "workspace_check.py",
        "legacy_map_check.py",
        "gen_contracts.py --check",
        "contract_compat.py",
        "bootstrap.sh",
        "pytest tests",
    ):
        assert needle in commands, f"baseline pipeline must run {needle}"


def test_workflow_runs_the_same_pipeline_and_uploads_the_summary():
    workflow = yaml.safe_load((ROOT / ".github" / "workflows" / "ci.yml").read_text())
    job = workflow["jobs"]["baseline"]
    steps = json.dumps(job["steps"])
    assert "scripts/ci/ci.sh" in steps
    assert "artifacts/ci/summary-*.json" in steps
    assert job["steps"][0]["with"]["fetch-depth"] == 0, "dossier consistency needs full history"
    script = ROOT / "scripts" / "ci" / "ci.sh"
    assert script.exists()
    assert script.stat().st_mode & 0o111, "ci.sh must be executable"


def test_no_release_blocking_test_is_skipped_or_xfailed():
    offenders = []
    for path in sorted((ROOT / "tests").rglob("test_*.py")):
        text = path.read_text()
        # Marker fragments are assembled here so this checker does not flag itself.
    markers = ("pytest.mark." + "skip", "pytest.mark." + "xfail", "@unittest." + "skip")
    for marker in markers:
            if marker in text:
                offenders.append(f"{path.relative_to(ROOT)}: {marker}")
    assert offenders == [], f"release-blocking tests must not be skipped: {offenders}"
