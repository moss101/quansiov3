#!/usr/bin/env python3
"""Collect a task evidence bundle (DOSSIER.md \u00a719).

Runs the given commands, captures their output, hashes the named artifacts and
writes `evidence/<TASK-ID>/<UTC timestamp>/summary.json` plus the raw logs.

Example:
  python3 scripts/dev/evidence.py --task GOV-001 \
      --command "python3 scripts/ci/inventory.py --scan" \
      --artifact docs/review/2026-09-12-gov-001-reconciliation.md

Python 3.9+ ; standard library only.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parents[2]


def utc_stamp(now: Optional[datetime] = None) -> str:
    """Evidence directory name: 2026-09-12T04-05-06Z (DOSSIER.md \u00a719)."""
    moment = now or datetime.now(timezone.utc)
    return moment.strftime("%Y-%m-%dT%H-%M-%SZ")


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def relative_to_root(path: Path, root: Path) -> str:
    """Repository-relative path when inside `root`, absolute otherwise (tests use a temp root)."""
    try:
        return str(path.relative_to(root))
    except ValueError:
        return str(path)


def git_commit(root: Path = ROOT) -> Optional[str]:
    try:
        return subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=str(root), capture_output=True, text=True, check=True
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def run_command(command: str, cwd: Path) -> Dict[str, object]:
    proc = subprocess.run(command, cwd=str(cwd), shell=True, capture_output=True, text=True)
    return {
        "command": command,
        "exit_code": proc.returncode,
        "result": "PASS" if proc.returncode == 0 else "FAIL",
        "stdout": proc.stdout,
        "stderr": proc.stderr,
    }


def collect(
    task_id: str,
    commands: Sequence[str],
    artifacts: Sequence[str],
    out_dir: Optional[Path] = None,
    root: Path = ROOT,
    stamp: Optional[str] = None,
) -> Dict[str, object]:
    """Run commands, hash artifacts and write the bundle; returns summary.json content."""
    bundle = (out_dir / task_id / (stamp or utc_stamp())) if out_dir else (
        root / "evidence" / task_id / (stamp or utc_stamp())
    )
    bundle.mkdir(parents=True, exist_ok=True)

    results: List[Dict[str, object]] = []
    for i, command in enumerate(commands, start=1):
        outcome = run_command(command, root)
        log = bundle / f"command-{i}.log"
        log.write_text(
            f"$ {command}\nexit_code: {outcome['exit_code']}\n\n--- stdout ---\n{outcome['stdout']}"
            f"\n--- stderr ---\n{outcome['stderr']}"
        )
        results.append(
            {
                "command": command,
                "exit_code": outcome["exit_code"],
                "result": outcome["result"],
                "log": relative_to_root(log, root),
            }
        )

    resolved_artifacts: List[Dict[str, object]] = []
    for rel in artifacts:
        path = root / rel
        if path.exists():
            resolved_artifacts.append({"path": rel, "sha256": sha256_file(path), "bytes": path.stat().st_size})
        else:
            resolved_artifacts.append({"path": rel, "sha256": None, "bytes": None})

    summary: Dict[str, object] = {
        "task_id": task_id,
        "git_commit": git_commit(root),
        "recorded_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "commands": results,
        "results": [
            {"command": r["command"], "exit_code": r["exit_code"], "result": r["result"]} for r in results
        ],
        "artifacts": resolved_artifacts,
    }
    (bundle / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    summary["bundle"] = relative_to_root(bundle, root)
    return summary


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Collect a Quansio task evidence bundle")
    ap.add_argument("--task", required=True, help="task id, e.g. GOV-001")
    ap.add_argument("--command", action="append", default=[], help="command to run (repeatable)")
    ap.add_argument("--artifact", action="append", default=[], help="repository-relative artifact (repeatable)")
    ap.add_argument("--out-dir", default=None, help="override the evidence root (tests)")
    ap.add_argument("--stamp", default=None, help="override the bundle timestamp, e.g. 2026-09-12T04-00-00Z")
    args = ap.parse_args(argv)

    summary = collect(
        args.task,
        args.command,
        args.artifact,
        out_dir=Path(args.out_dir) if args.out_dir else None,
        stamp=args.stamp,
    )
    failed = [r for r in summary["commands"] if r["exit_code"] != 0]  # type: ignore[index]
    print(json.dumps(summary, indent=2))
    if failed:
        print(f"evidence collection FAILED: {len(failed)} command(s) exited non-zero", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
