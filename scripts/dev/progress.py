#!/usr/bin/env python3
"""Update one task's entry in `registries/progress.json` (AGENTS.md task loop).

  python3 scripts/dev/progress.py --task GOV-001 --status RECONCILING --claimed-by agent:principal-1
  python3 scripts/dev/progress.py --task GOV-001 --status PASS --coverage GENUINE_GAP \
      --git-commit <hash> --test "python3 -m pytest tests/architecture" \
      --artifact evidence/GOV-001/<ts>/summary.json --implementation-complete

The tool writes only the fields named on the command line; it never rewrites the
whole file's semantic content. `registries/progress.json` remains the canonical
progress record (DOSSIER.md \u00a719-\u00a720); this is the editing tool, not a second store.

Python 3.9+ ; standard library only.
"""
from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parents[2]
PROGRESS = ROOT / "registries" / "progress.json"

STATUSES = {
    "NOT_STARTED",
    "RECONCILING",
    "IN_PROGRESS",
    "BLOCKED_EXTERNAL",
    "BLOCKED_CONFLICT",
    "PASS",
    "FAIL",
    "DEFERRED_NON_GA",
}
COVERAGE = {
    "ALREADY_COVERED",
    "PARTIAL",
    "GENUINE_GAP",
    "SUPERSEDED",
    "CONFLICT",
    "IMPLEMENTED_UNDOCUMENTED",
}


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def update(
    task_id: str,
    status: Optional[str] = None,
    claimed_by: Optional[str] = None,
    coverage: Optional[str] = None,
    git_commit: Optional[str] = None,
    tests: Optional[Sequence[str]] = None,
    artifacts: Optional[Sequence[str]] = None,
    real_boundary_evidence: Optional[Sequence[dict]] = None,
    implementation_complete: Optional[bool] = None,
    blocker: Optional[str] = None,
    notes: Optional[str] = None,
    progress_path: Path = PROGRESS,
    now: Optional[str] = None,
) -> Dict[str, object]:
    """Apply the requested field updates and return the task's new progress entry."""
    data = json.loads(progress_path.read_text())
    tasks = data["tasks"]
    if task_id not in tasks:
        raise KeyError(f"unknown task {task_id}")

    if status is not None and status not in STATUSES:
        raise ValueError(f"invalid status {status!r}")
    if coverage is not None and coverage not in COVERAGE:
        raise ValueError(f"invalid coverage {coverage!r}")
    if status == "NOT_STARTED" and any(
        [claimed_by, git_commit, implementation_complete, tests, artifacts, coverage]
    ):
        raise ValueError("NOT_STARTED must not carry claim/commit/evidence fields")

    entry = tasks[task_id]
    stamp = now or utc_now()
    if status is not None:
        entry["status"] = status
        entry["updated_at"] = stamp
        if status != "NOT_STARTED" and not entry["started_at"]:
            entry["started_at"] = stamp
    if claimed_by is not None:
        entry["claimed_by"] = claimed_by
    if coverage is not None:
        entry["coverage"] = coverage
    if git_commit is not None:
        entry["git_commit"] = git_commit
    if tests is not None:
        entry["tests"] = list(tests)
    if artifacts is not None:
        entry["artifacts"] = list(artifacts)
    if real_boundary_evidence is not None:
        entry["real_boundary_evidence"] = list(real_boundary_evidence)
    if implementation_complete is not None:
        entry["implementation_complete"] = bool(implementation_complete)
    if blocker is not None:
        entry["blocker"] = blocker or None
    if notes is not None:
        entry["notes"] = notes

    if entry["status"] != "NOT_STARTED" and not entry["updated_at"]:
        entry["updated_at"] = stamp
    data["updated_at"] = stamp[:10]
    progress_path.write_text(json.dumps(data, indent=2) + "\n")
    return entry


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Update registries/progress.json for one task")
    ap.add_argument("--task", required=True)
    ap.add_argument("--status", choices=sorted(STATUSES))
    ap.add_argument("--claimed-by")
    ap.add_argument("--coverage", choices=sorted(COVERAGE))
    ap.add_argument("--git-commit")
    ap.add_argument("--test", action="append", dest="tests")
    ap.add_argument("--artifact", action="append", dest="artifacts")
    ap.add_argument("--implementation-complete", action="store_true", default=None)
    ap.add_argument("--blocker")
    ap.add_argument("--notes")
    ap.add_argument("--json", action="store_true", help="print the updated entry")
    args = ap.parse_args(argv)

    try:
        entry = update(
            args.task,
            status=args.status,
            claimed_by=args.claimed_by,
            coverage=args.coverage,
            git_commit=args.git_commit,
            tests=args.tests,
            artifacts=args.artifacts,
            implementation_complete=args.implementation_complete,
            blocker=args.blocker,
            notes=args.notes,
        )
    except (KeyError, ValueError) as exc:
        print(f"progress update rejected: {exc}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(entry, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
