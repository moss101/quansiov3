#!/usr/bin/env python3
"""Authority-pointer and archive-exclusion checks (GOV-002).

Enforces that the repository's active implementation instructions reference only
the V8.1 authority set, and that superseded material under `docs/archive/` cannot
be consumed as current task authority (DOSSIER.md \u00a71).

  python3 scripts/ci/check_authority.py --check          exit 1 on any finding
  python3 scripts/ci/check_authority.py --check --json   machine-readable findings

Python 3.9+ ; standard library only.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parents[2]

# Files that instruct humans/agents how to implement. They must not point at
# superseded material as if it were authority.
ACTIVE_INSTRUCTION_FILES = [
    "README.md",
    "AGENTS.md",
    "IMPLEMENTATION_MASTER_PROMPT.md",
    "HANDOFF.md",
]

# The authority set that the root README must name (DOSSIER.md \u00a71).
REQUIRED_README_REFERENCES = [
    "DOSSIER.md",
    "DOMAIN.md",
    "AGENTS.md",
    "registries/tasks.json",
    "registries/progress.json",
]

ARCHIVE_DIR = "docs/archive"
BANNER = "NON-AUTHORITY"
ARCHIVE_REF_RE = re.compile(r"docs/archive/([A-Za-z0-9._/-]+)")


def check(root: Path = ROOT) -> List[Dict[str, object]]:
    """Return one finding per authority-pointer / archive-exclusion violation."""
    root = Path(root)
    findings: List[Dict[str, object]] = []

    def add(rule: str, path: str, detail: str) -> None:
        findings.append({"rule": rule, "path": path, "detail": detail})

    readme = root / "README.md"
    if not readme.exists():
        add("missing-authority-pointer", "README.md", "root README must exist and point at the V8.1 authority set")
    else:
        text = readme.read_text(errors="replace")
        for ref in REQUIRED_README_REFERENCES:
            if ref not in text:
                add("missing-authority-pointer", "README.md", f"README must name authority artifact {ref}")

    for rel in ACTIVE_INSTRUCTION_FILES:
        path = root / rel
        if not path.exists():
            continue
        text = path.read_text(errors="replace")
        for match in ARCHIVE_REF_RE.finditer(text):
            target_rel = f"{ARCHIVE_DIR}/{match.group(1).rstrip('/')}"
            target = root / target_rel
            if not target.exists():
                continue
            if target.is_dir():
                continue
            body = target.read_text(errors="replace").splitlines()[:40]
            if not any(BANNER in line for line in body):
                add(
                    "archive-reference-without-banner",
                    rel,
                    f"references {target_rel} which does not declare {BANNER}",
                )

    archive = root / ARCHIVE_DIR
    if archive.is_dir():
        for path in sorted(p for p in archive.rglob("*") if p.is_file()):
            rel = str(path.relative_to(root))
            head = path.read_text(errors="replace").splitlines()[:40]
            if not any(BANNER in line for line in head):
                add(
                    "archive-file-without-banner",
                    rel,
                    f"archived file must declare {BANNER} in its first 40 lines",
                )
    return findings


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Authority-pointer and archive-exclusion checks")
    ap.add_argument("--check", action="store_true", help="run the checks (default)")
    ap.add_argument("--json", action="store_true", help="emit findings as JSON")
    ap.add_argument("--root", default=None, help="tree to check (defaults to the repository root)")
    args = ap.parse_args(argv)

    findings = check(Path(args.root) if args.root else ROOT)
    if args.json:
        print(json.dumps(findings, indent=2))
    elif not findings:
        print("authority check: CLEAN (0 findings)")
    else:
        for f in findings:
            print(f"{f['rule']}: {f['path']}: {f['detail']}")
        print(f"authority check: {len(findings)} finding(s)")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
