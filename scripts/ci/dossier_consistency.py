#!/usr/bin/env python3
"""Dossier consistency validation (GOV-005).

Extends the authority validator with the facts that need git and the filesystem: a
`PASS` claim must be provable from the repository alone.

Checks:
 1. `python3 scripts/validate_v81.py` passes (authority set, registries, generated views).
 2. every `PASS` task's `git_commit` is a real commit reachable from `main`;
 3. every `PASS` task's declared `artifacts` exist in the worktree;
 4. every `PASS` task's evidence bundle is a valid DOSSIER §19 summary and its recorded
    commit is itself reachable from `main`;
 5. `PASS` tasks declare at least one test command.

  python3 scripts/ci/dossier_consistency.py            human-readable
  python3 scripts/ci/dossier_consistency.py --json     machine-readable findings

Python 3.11+ ; standard library only.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parents[2]
PROGRESS = "registries/progress.json"
EVIDENCE_SCHEMA = "schemas/json/evidence-summary.schema.json"


def _git(args: Sequence[str], root: Path) -> Optional[str]:
    try:
        result = subprocess.run(
            ["git", *args], cwd=str(root), capture_output=True, text=True, check=True
        )
        return result.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def commit_reachable(commit: str, root: Path, branch: str = "main") -> bool:
    if _git(["cat-file", "-e", f"{commit}^{{commit}}"], root) is None:
        return False
    if _git(["rev-parse", "--verify", branch], root) is None:
        # No main branch yet (fresh repository): existence is the strongest claim available.
        return True
    return _git(["merge-base", "--is-ancestor", commit, branch], root) is not None


REQUIRED_SUMMARY_FIELDS = ("task_id", "commands", "results", "artifacts")


def validate_evidence(summary: Dict[str, object]) -> List[str]:
    """Validate an evidence summary against the generated DOSSIER §19 JSON Schema.

    Falls back to the schema's required-field contract when `jsonschema` is not
    installed, so the gate still runs on a bare interpreter instead of reporting a
    finding for every bundle.
    """
    schema_path = ROOT / EVIDENCE_SCHEMA
    if not schema_path.exists():
        return [f"{EVIDENCE_SCHEMA} missing"]
    schema = json.loads(schema_path.read_text())
    try:
        from jsonschema import Draft202012Validator
    except ModuleNotFoundError:
        missing = [field for field in schema.get("required", REQUIRED_SUMMARY_FIELDS) if field not in summary]
        return [f"evidence summary missing {field}" for field in missing]
    errors = sorted(Draft202012Validator(schema).iter_errors(summary), key=lambda e: list(e.path))
    return [f"evidence summary invalid: {error.message}" for error in errors]


def check(root: Optional[Path] = None, run_validator: bool = True) -> List[Dict[str, str]]:
    """Return one finding per dossier inconsistency; empty list means consistent."""
    root = Path(root) if root is not None else ROOT
    findings: List[Dict[str, str]] = []

    def add(rule: str, path: str, detail: str) -> None:
        findings.append({"rule": rule, "path": path, "detail": detail})

    if run_validator:
        result = subprocess.run(
            [sys.executable, str(root / "scripts" / "validate_v81.py")],
            cwd=str(root),
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            for line in (result.stdout + result.stderr).splitlines():
                if line.strip().startswith("-"):
                    add("authority-validator", "registries", line.strip(" -"))

    progress_path = root / PROGRESS
    if not progress_path.exists():
        add("missing-progress", PROGRESS, "progress registry is missing")
        return findings
    progress = json.loads(progress_path.read_text())

    for task_id, entry in progress.get("tasks", {}).items():
        if entry.get("status") != "PASS":
            continue
        commit = entry.get("git_commit")
        if not commit:
            add("pass-without-commit", task_id, "PASS task must record git_commit")
        elif not commit_reachable(str(commit), root):
            add(
                "unreachable-commit",
                task_id,
                f"git_commit {commit} is not reachable from main",
            )
        if not entry.get("tests"):
            add("pass-without-tests", task_id, "PASS task must record the tests it ran")
        for artifact in entry.get("artifacts", []):
            if not (root / artifact).exists():
                add("missing-artifact", task_id, f"artifact not found: {artifact}")
        bundles = sorted((root / "evidence" / task_id).glob("*/summary.json"))
        if not bundles:
            add("missing-evidence", task_id, "no evidence/<task>/<ts>/summary.json committed")
        for bundle in bundles:
            summary = json.loads(bundle.read_text())
            for problem in validate_evidence(summary):
                add("invalid-evidence", str(bundle.relative_to(root)), problem)
            recorded = summary.get("git_commit")
            if recorded and not commit_reachable(str(recorded), root):
                add(
                    "unreachable-evidence-commit",
                    str(bundle.relative_to(root)),
                    f"evidence commit {recorded} is not reachable from main",
                )
    return findings


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Dossier consistency validation")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--root", default=None)
    ap.add_argument("--skip-validator", action="store_true")
    args = ap.parse_args(argv)

    findings = check(Path(args.root) if args.root else ROOT, run_validator=not args.skip_validator)
    if args.json:
        print(json.dumps(findings, indent=2))
    elif not findings:
        print("dossier consistency: CLEAN (0 findings)")
    else:
        for finding in findings:
            print(f"{finding['rule']}: {finding['path']}: {finding['detail']}")
        print(f"dossier consistency: {len(findings)} finding(s)")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
