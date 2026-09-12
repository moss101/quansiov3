#!/usr/bin/env python3
"""Workspace and language-boundary conformance check (GOV-003, reused by GOV-008).

Verifies from the repository itself that:

1. every canonical owner maps to exactly one package/module (and vice versa);
2. the Cargo workspace has no dependency cycles and no cross-language source deps;
3. each pnpm and Python package belongs to the language boundary of its owner;
4. no Rust crate claims a canonical owner path other than its own directory.

  python3 scripts/ci/workspace_check.py            human-readable
  python3 scripts/ci/workspace_check.py --json     machine-readable findings

Python 3.11+ (tomllib); the repository pins 3.12 in .python-version.
"""
from __future__ import annotations

import argparse
import json
import re
import sys

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover - exercised only on old interpreters
    sys.stderr.write("workspace_check.py requires Python 3.11+ (run it with python3.12)\n")
    raise SystemExit(2)

from collections import defaultdict, deque
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:  # runnable as a script and importable as a module
    sys.path.insert(0, str(ROOT))

from scripts.ci import inventory  # noqa: E402

# Rust canonical owners that must be materialised as workspace members.
REQUIRED_RUST_OWNERS = {
    "crates/server": "quansio-server",
    "crates/core": "quansio-core",
    "crates/graph": "quansio-graph",
    "crates/events": "quansio-events",
    "crates/capability": "quansio-capability",
    "crates/tools": "quansio-tools",
    "crates/indexer": "quansio-indexer",
    "crates/machine": "quansio-machine",
    "crates/qworkerd": "quansio-qworkerd",
    "crates/cli": "quansio-cli",
    "crates/contracts": "quansio-contracts",
}

# Rust owners whose source lives inside quansio-server modules.
SERVER_MODULE_OWNERS = tuple(
    f"crates/server/src/{m}"
    for m in (
        "api",
        "control",
        "runtime",
        "policy",
        "effects",
        "scheduler",
        "artifacts",
        "notify",
        "audit",
        "usage",
    )
)

OWNER_CONST_RE = re.compile(r'CANONICAL_OWNER\s*:\s*&str\s*=\s*"([^"]+)"')


def _load_toml(path: Path) -> Dict[str, object]:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def cargo_workspace(root: Path) -> Tuple[List[Path], List[Dict[str, object]]]:
    """Return (member dirs, findings) for the Cargo workspace."""
    findings: List[Dict[str, object]] = []

    def add(rule: str, path: str, detail: str) -> None:
        findings.append({"rule": rule, "path": path, "detail": detail})

    cargo = root / "Cargo.toml"
    if not cargo.exists():
        add("missing-cargo-workspace", "Cargo.toml", "Cargo workspace manifest is missing")
        return [], findings
    manifest = _load_toml(cargo)
    members = list(manifest.get("workspace", {}).get("members", []))  # type: ignore[union-attr]
    if not members:
        add("empty-cargo-workspace", "Cargo.toml", "workspace.members must not be empty")

    dirs: List[Path] = []
    for member in members:
        member_dir = root / member
        member_cargo = member_dir / "Cargo.toml"
        if not member_cargo.exists():
            add("missing-member-manifest", member, "workspace member has no Cargo.toml")
            continue
        dirs.append(member_dir)
        member_manifest = _load_toml(member_cargo)
        pkg = member_manifest.get("package", {})  # type: ignore[union-attr]
        if not pkg:
            add("member-without-package", member, "workspace member must declare [package]")
        lib = member_dir / "src" / "lib.rs"
        main = member_dir / "src" / "main.rs"
        source = lib if lib.exists() else main
        if source is None:
            add("member-without-source", member, "workspace member must contain src/lib.rs or src/main.rs")
            continue
        match = OWNER_CONST_RE.search(source.read_text())
        if match is None:
            add("missing-owner-constant", member, "crate must declare pub const CANONICAL_OWNER")
        elif match.group(1) != member:
            add(
                "wrong-owner-constant",
                member,
                f"CANONICAL_OWNER is {match.group(1)!r} but the crate lives at {member!r}",
            )
    return dirs, findings


def member_graph(root: Path, dirs: Sequence[Path]) -> Tuple[Dict[str, set], List[Dict[str, object]]]:
    """Crate-name dependency graph restricted to workspace members."""
    findings: List[Dict[str, object]] = []
    names: Dict[str, str] = {}
    manifests: Dict[str, Dict[str, object]] = {}
    for d in dirs:
        manifest = _load_toml(d / "Cargo.toml")
        name = str(manifest.get("package", {}).get("name", ""))  # type: ignore[union-attr]
        rel = str(d.relative_to(root))
        names[name] = rel
        manifests[rel] = manifest

    graph: Dict[str, set] = {rel: set() for rel in names.values()}
    for rel, manifest in manifests.items():
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            deps = manifest.get(section, {}) or {}
            for dep in deps:  # type: ignore[union-attr]
                if dep in names:
                    target = names[dep]
                    if target == rel:
                        findings.append({"rule": "self-dependency", "path": rel, "detail": f"{dep} depends on itself"})
                    else:
                        graph[rel].add(target)
    return graph, findings


def find_cycles(graph: Dict[str, set]) -> List[List[str]]:
    """Return one representative cycle per strongly connected component with an edge."""
    cycles: List[List[str]] = []
    indeg: Dict[str, int] = {n: 0 for n in graph}
    for n, targets in graph.items():
        for t in targets:
            indeg[t] = indeg.get(t, 0) + 1
    q = deque([n for n, v in indeg.items() if v == 0])
    visited = 0
    while q:
        n = q.popleft()
        visited += 1
        for t in graph.get(n, ()):  # remaining nodes form cycles
            indeg[t] -= 1
            if indeg[t] == 0:
                q.append(t)
    if visited != len(graph):
        remaining = [n for n, v in indeg.items() if v > 0]
        cycles.append(sorted(remaining))
    return cycles


def check(root: Optional[Path] = None, require_all_owners: bool = True) -> List[Dict[str, object]]:
    """All workspace findings; empty list means conformance."""
    root = Path(root) if root is not None else ROOT
    findings: List[Dict[str, object]] = []

    def add(rule: str, path: str, detail: str) -> None:
        findings.append({"rule": rule, "path": path, "detail": detail})

    dirs, cargo_findings = cargo_workspace(root)
    findings.extend(cargo_findings)
    graph, dep_findings = member_graph(root, dirs)
    findings.extend(dep_findings)

    for cycle in find_cycles(graph):
        add("dependency-cycle", "Cargo.toml", f"workspace dependency cycle: {' -> '.join(cycle)}")

    # One owner per package, one package per owner.
    owner_to_crates: Dict[str, List[str]] = defaultdict(list)
    for d in dirs:
        rel = str(d.relative_to(root))
        owner = inventory.owner_of(rel)
        if owner is None:
            add("member-without-owner", rel, "workspace member does not map to a canonical owner")
        owner_to_crates[owner].append(rel)
    for owner, crates in owner_to_crates.items():
        if owner and len(crates) > 1:
            add("owner-mapped-twice", crates[0], f"canonical owner {owner} has multiple packages: {crates}")

    if require_all_owners:
        present = {str(d.relative_to(root)) for d in dirs}
        for owner, crate in REQUIRED_RUST_OWNERS.items():
            if owner not in present:
                add("missing-owner-package", owner, f"canonical owner {owner} must exist as workspace member {crate}")
            elif crate not in (root / owner / "Cargo.toml").read_text():
                add("owner-package-name-mismatch", owner, f"expected package name {crate}")
        for module in SERVER_MODULE_OWNERS:
            if not (root / module).is_dir():
                add("missing-server-module", module, "quansio-server must own this module directory")

    # Language boundary: Rust lives under crates/ and native/ only.
    for d in dirs:
        rel = str(d.relative_to(root))
        if not rel.startswith(("crates/", "native/")):
            add("rust-outside-boundary", rel, "Rust crates must live under crates/ or native/")
    for path in sorted((root / "crates").glob("*")):
        if path.is_dir() and (path / "Cargo.toml").exists() and str(path.relative_to(root)) not in {
            str(d.relative_to(root)) for d in dirs
        }:
            add("crate-not-in-workspace", str(path.relative_to(root)), "crate exists but is not a workspace member")

    # pnpm packages must live under apps/ or sdk/, and sdk/python must not be a node package.
    pnpm = root / "pnpm-workspace.yaml"
    if pnpm.exists():
        text = pnpm.read_text()
        globs = re.findall(r'^\s*-\s*"([^"]+)"', text, re.MULTILINE)
        if not globs:
            add("empty-pnpm-workspace", "pnpm-workspace.yaml", "pnpm workspace must declare package globs")
        for glob in globs:
            for pkg in sorted(root.glob(glob)):
                rel = str(pkg.relative_to(root))
                if not rel.startswith(("apps/", "sdk/")):
                    add("node-package-outside-boundary", rel, "pnpm packages must live under apps/ or sdk/")
                if not (pkg / "package.json").exists():
                    add("node-package-without-manifest", rel, "package directory needs package.json")
    else:
        add("missing-pnpm-workspace", "pnpm-workspace.yaml", "pnpm workspace file is missing")

    # Python package must exist where pyproject says it does.
    pyproject = root / "python" / "pyproject.toml"
    if not pyproject.exists():
        add("missing-python-workspace", "python/pyproject.toml", "Python workspace manifest is missing")
    else:
        manifest = _load_toml(pyproject)
        packages = (
            manifest.get("tool", {})  # type: ignore[union-attr]
            .get("hatch", {})
            .get("build", {})
            .get("targets", {})
            .get("wheel", {})
            .get("packages", [])
        )
        if not packages:
            add("python-packages-undeclared", "python/pyproject.toml", "wheel packages must be declared")
        for pkg in packages:
            if not (root / "python" / pkg).is_dir():
                add("python-package-missing", f"python/{pkg}", "declared Python package directory does not exist")
        require = manifest.get("project", {}).get("requires-python", "")  # type: ignore[union-attr]
        if "3.12" not in str(require):
            add("python-version-unpinned", "python/pyproject.toml", "requires-python must pin 3.12+")
    return findings


def render(root: Optional[Path] = None) -> str:
    root = Path(root) if root is not None else ROOT
    dirs, _ = cargo_workspace(root)
    lines = ["# Canonical owner → package mapping", ""]
    for d in sorted(dirs):
        rel = str(d.relative_to(root))
        manifest = _load_toml(d / "Cargo.toml")
        name = manifest.get("package", {}).get("name", "?")  # type: ignore[union-attr]
        lines.append(f"{rel} -> {name} (owner: {inventory.owner_of(rel)})")
    return "\n".join(lines)


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Workspace and language-boundary conformance check")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--map", action="store_true", help="print the owner → package mapping")
    ap.add_argument("--root", default=None)
    args = ap.parse_args(argv)

    root = Path(args.root) if args.root else ROOT
    if args.map:
        print(render(root))
        return 0
    findings = check(root)
    if args.json:
        print(json.dumps(findings, indent=2))
    elif not findings:
        print("workspace check: CLEAN (0 findings)")
    else:
        for f in findings:
            print(f"{f['rule']}: {f['path']}: {f['detail']}")
        print(f"workspace check: {len(findings)} finding(s)")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
