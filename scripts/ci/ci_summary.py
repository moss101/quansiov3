#!/usr/bin/env python3
"""Run the baseline pipeline and publish a commit-bound machine-readable summary (GOV-005).

The gate list here is the single definition of the baseline pipeline: `scripts/ci/ci.sh`
and `.github/workflows/ci.yml` both invoke this tool, so a clean checkout runs exactly
what CI runs.

  python3 scripts/ci/ci_summary.py                 run every gate, write the summary
  python3 scripts/ci/ci_summary.py --json          also print the summary
  python3 scripts/ci/ci_summary.py --command "…"   override the gates (used by tests)
  python3 scripts/ci/ci_summary.py --out <path>    choose the summary location

The summary binds each result to the commit, the gate command, its exit code, its
duration and the digests of the artifacts it produced.

Python 3.11+ ; standard library only.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUT_DIR = ROOT / "artifacts" / "ci"

# name -> command. Each gate is a real entry point that fails non-zero on real problems.
GATES: List[tuple] = [
    ("authority", "python3.12 scripts/validate_v81.py"),
    ("dossier-consistency", "python3.12 scripts/ci/dossier_consistency.py"),
    ("architecture", "python3.12 scripts/ci/arch_check.py"),
    ("authority-pointers", "python3.12 scripts/ci/check_authority.py --check"),
    ("workspace", "python3.12 scripts/ci/workspace_check.py"),
    ("legacy-map", "python3.12 scripts/ci/legacy_map_check.py"),
    ("contract-drift", "uv run --project python python scripts/ci/gen_contracts.py --check"),
    ("contract-lint-compat", "uv run --project python python scripts/ci/contract_compat.py"),
    ("toolchains", "bash scripts/dev/bootstrap.sh"),
    ("tests", "uv run --project python pytest tests -q"),
]
# `arch_check.py` is created by GOV-008; until it exists the gate is skipped explicitly
# (never silently passed) and reported as `SKIPPED_PENDING` in the summary.
OPTIONAL_GATES = {"architecture": "scripts/ci/arch_check.py"}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_commit(root: Path) -> Optional[str]:
    try:
        return subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=str(root), capture_output=True, text=True, check=True
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def run_gate(name: str, command: str, root: Path, timeout: int = 3600) -> Dict[str, object]:
    started = time.monotonic()
    try:
        result = subprocess.run(
            command, cwd=str(root), shell=True, capture_output=True, text=True, timeout=timeout
        )
        exit_code: Optional[int] = result.returncode
        tail = "\n".join((result.stdout + result.stderr).splitlines()[-25:])
    except subprocess.TimeoutExpired:
        exit_code = None
        tail = f"gate timed out after {timeout}s"
    duration = round(time.monotonic() - started, 3)
    status = "PASS" if exit_code == 0 else ("TIMEOUT" if exit_code is None else "FAIL")
    return {
        "gate": name,
        "command": command,
        "status": status,
        "exit_code": exit_code,
        "duration_s": duration,
        "tail": tail,
    }


def run(
    commands: Optional[Sequence[str]] = None,
    out: Optional[Path] = None,
    root: Path = ROOT,
    gates: Optional[Sequence[tuple]] = None,
) -> Dict[str, object]:
    """Run the gates and write the summary; returns the summary object."""
    if commands:
        gates = [(f"gate-{i}", command) for i, command in enumerate(commands, start=1)]
    elif gates is None:
        gates = GATES
    results: List[Dict[str, object]] = []
    for name, command in gates:
        if name in OPTIONAL_GATES:
            target = root / OPTIONAL_GATES[name]
            if not target.exists():
                results.append(
                    {
                        "gate": name,
                        "command": command,
                        "status": "SKIPPED_PENDING",
                        "exit_code": None,
                        "duration_s": 0.0,
                        "tail": f"{OPTIONAL_GATES[name]} not present yet",
                    }
                )
                continue
        results.append(run_gate(name, command, root))

    artifacts: Dict[str, str] = {}
    for relative in ("registries/progress.json", "TASKS.md", "MANIFEST.json"):
        path = root / relative
        if path.exists():
            artifacts[relative] = sha256_file(path)

    summary = {
        "commit": git_commit(root),
        "recorded_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "results": results,
        "artifacts": artifacts,
        "status": "FAIL" if any(r["status"] == "FAIL" or r["status"] == "TIMEOUT" for r in results) else "PASS",
    }
    destination = out or (DEFAULT_OUT_DIR / f"summary-{(summary['commit'] or 'nogit')[:12]}.json")
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(summary, indent=2) + "\n")
    summary["summary_path"] = str(destination)
    return summary


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Run the baseline pipeline and publish a summary")
    ap.add_argument("--command", action="append", default=[], help="override the gate list (repeatable)")
    ap.add_argument("--out", default=None, help="summary output path")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    summary = run(args.command, Path(args.out) if args.out else None)
    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        for result in summary["results"]:
            print(f"{result['status']:<16} {result['gate']:<22} {result['command']}")
        print(f"summary: {summary['summary_path']}")
        print(f"pipeline: {summary['status']}")
    return 0 if summary["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
