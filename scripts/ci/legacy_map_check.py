#!/usr/bin/env python3
"""Legacy migration-map completeness and direct-path scan (GOV-006).

The V8.1 rule is that useful legacy code may be kept, ported, replaced or deleted, but
never left as an unowned parallel authority (AGENTS.md "Existing code and greenfield",
DOSSIER.md §20). This checker makes that rule executable:

1. every legacy artifact found in the tree must have a row in
   `docs/review/legacy-migration-map.md` with a valid disposition;
2. a `DELETE`/`REPLACE` row must no longer be present in the tree;
3. a `KEEP_BEHIND_BOUNDARY`/`PORT` row must still exist and name a canonical V8.1 owner;
4. the direct-path scan (no provider SDK outside the model gateway, no tool registration
   outside the Rust Tool Registry, no Python/TS authority writes) must be clean.

  python3 scripts/ci/legacy_map_check.py            human-readable
  python3 scripts/ci/legacy_map_check.py --json     machine-readable findings

Python 3.11+ ; standard library only.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from scripts.ci import inventory  # noqa: E402

MAP_PATH = "docs/review/legacy-migration-map.md"
DISPOSITIONS = {"KEEP_BEHIND_BOUNDARY", "PORT", "REPLACE", "DELETE"}

# Artifacts that indicate legacy or parallel-authority code, by path or suffix.
LEGACY_PATTERNS = (
    re.compile(r"^(?:legacy|old|src/legacy)/"),
    re.compile(r"\.go$"),
    re.compile(r"(?:^|/)(?:orchestrator|scheduler_v1|runtime_v1)\.(?:py|ts|tsx|go|rs|js)$"),
    re.compile(r"(?:^|/)server\.py$"),  # legacy FastAPI-style entry point
)

ROW_RE = re.compile(r"^\|\s*`?([^|`]+?)`?\s*\|\s*([A-Z_]+)\s*\|\s*`?([^|`]+?)`?\s*\|\s*$", re.MULTILINE)


def parse_map(root: Path) -> List[Dict[str, str]]:
    path = root / MAP_PATH
    if not path.exists():
        return []
    rows: List[Dict[str, str]] = []
    for match in ROW_RE.finditer(path.read_text()):
        artifact, disposition, owner = (part.strip() for part in match.groups())
        if artifact.lower() in {"artifact", "path"} or disposition not in DISPOSITIONS:
            continue
        rows.append({"artifact": artifact, "disposition": disposition, "owner": owner})
    return rows


def _files(root: Path) -> List[str]:
    """Repository-relative files under `root` (git-tracked for the real repository)."""
    if root == inventory.ROOT:
        return inventory.tracked_files()
    return sorted(
        str(p.relative_to(root)) for p in root.rglob("*") if p.is_file() and ".git/" not in str(p)
    )


def legacy_artifacts(root: Path) -> List[str]:
    found: List[str] = []
    for rel in _files(root):
        if any(p.search(rel) for p in LEGACY_PATTERNS):
            found.append(rel)
    return sorted(found)


def check(root: Optional[Path] = None) -> List[Dict[str, object]]:
    root = Path(root) if root is not None else ROOT
    findings: List[Dict[str, object]] = []

    def add(rule: str, path: str, detail: str) -> None:
        findings.append({"rule": rule, "path": path, "detail": detail})

    map_path = root / MAP_PATH
    if not map_path.exists():
        add("missing-migration-map", MAP_PATH, "legacy migration map must exist (GOV-006)")
        return findings

    rows = parse_map(root)
    mapped = {row["artifact"]: row for row in rows}

    for row in rows:
        artifact = row["artifact"]
        exists = (root / artifact).exists()
        if row["disposition"] in {"DELETE", "REPLACE"} and exists:
            add(
                "disposition-path-still-present",
                artifact,
                f"{row['disposition']} disposition but the artifact still exists",
            )
        if row["disposition"] in {"KEEP_BEHIND_BOUNDARY", "PORT"}:
            if not exists:
                add("kept-artifact-missing", artifact, "KEEP/PORT row names an artifact that does not exist")
            if inventory.owner_of(row["owner"]) is None and not row["owner"].startswith(("crates/", "python/", "apps/", "native/", "sdk/", "infra/")):
                add(
                    "kept-artifact-owner-unknown",
                    artifact,
                    f"canonical owner {row['owner']!r} is not a V8.1 owner path",
                )

    for artifact in legacy_artifacts(root):
        if artifact not in mapped:
            add(
                "legacy-without-disposition",
                artifact,
                "legacy/parallel-authority artifact has no disposition row in the migration map",
            )

    scan_files = [
        f
        for f in _files(root)
        if inventory.is_product_code(f) and not f.startswith("tests/")
    ]
    for finding in inventory.scan(files=scan_files, root=root):
        add(
            "legacy-direct-path",
            str(finding["path"]),
            f"direct/parallel path ({finding['rule']}): {finding['detail']}",
        )
    return findings


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Legacy migration-map completeness and direct-path scan")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--root", default=None)
    args = ap.parse_args(argv)

    findings = check(Path(args.root) if args.root else ROOT)
    if args.json:
        print(json.dumps(findings, indent=2))
    elif not findings:
        print("legacy map check: CLEAN (0 findings)")
    else:
        for f in findings:
            print(f"{f['rule']}: {f['path']}: {f['detail']}")
        print(f"legacy map check: {len(findings)} finding(s)")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
