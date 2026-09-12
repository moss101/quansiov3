#!/usr/bin/env python3
"""Repository inventory and duplicate-authority scan (GOV-001).

Produces the factual baseline required before any V8.1 implementation:

  python3 scripts/ci/inventory.py                 human-readable inventory
  python3 scripts/ci/inventory.py --json          machine-readable inventory
  python3 scripts/ci/inventory.py --scan          duplicate-authority scan (exit 1 on violation)

The scanner is the executable part of the reconciliation: it maps every
product-code file to its V8.1 canonical owner (DOSSIER.md \u00a717) and fails when
code exists that can mutate durable state or cause effects without an owner,
or that takes authority that a locked language boundary (DOSSIER.md \u00a73) denies.

Python 3.9+ ; standard library only.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

ROOT = Path(__file__).resolve().parents[2]

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
    "scripts/validate_v81.py",
]

# Paths that are never product code: authority set, generated evidence, tooling
# metadata, dependency and build output directories.
EXCLUDED_PREFIXES = (
    "docs/",
    "evidence/",
    "registries/",
    "manifest",
    ".git/",
    ".github/",
    "node_modules/",
    "target/",
    ".venv/",
    "dist/",
    "build/",
    "scripts/",
)
EXCLUDED_SUFFIXES = (".pyc", ".pyo", ".lock", ".min.js", ".map")
EXCLUDED_FILES = set(AUTHORITY_FILES) | {
    "scripts/validate_v81.py",
    "scripts/ci/inventory.py",
    "scripts/ci/arch_check.py",
}

LANG_BY_SUFFIX = {
    ".rs": "rust",
    ".py": "python",
    ".ts": "typescript",
    ".tsx": "typescript",
    ".js": "javascript",
    ".swift": "swift",
    ".go": "go",
    ".sql": "sql",
    ".proto": "protobuf",
    ".yaml": "yaml",
    ".yml": "yaml",
}

# Canonical owner -> path prefixes (DOSSIER.md \u00a75 ownership table, \u00a717 layout).
CANONICAL_OWNERS: Dict[str, Tuple[str, ...]] = {
    "quansio-server": ("crates/server/",),
    "core": ("crates/core/",),
    "graph": ("crates/graph/",),
    "events": ("crates/events/",),
    "capability": ("crates/capability/",),
    "policy": ("crates/server/policy/", "crates/policy/"),
    "effects": ("crates/server/effects/", "crates/effects/"),
    "tools": ("crates/tools/",),
    "indexer": ("crates/indexer/",),
    "machine-control": ("crates/machine/",),
    "qworkerd": ("crates/qworkerd/",),
    "cli": ("crates/cli/",),
    "contracts": ("crates/contracts/", "schemas/"),
    "intelligence": ("python/intelligence/",),
    "model-gateway": ("python/intelligence/model_gateway/",),
    "context": ("python/intelligence/context/",),
    "knowledge": ("python/intelligence/knowledge/",),
    "memory": ("python/intelligence/memory/",),
    "embeddings": ("python/intelligence/embeddings/",),
    "trust": ("python/intelligence/trust/",),
    "skills": ("python/intelligence/skills/",),
    "capability-compiler": ("python/intelligence/capability_compiler/",),
    "evaluation": ("python/intelligence/evaluation/",),
    "artifacts-adapter": ("python/intelligence/artifacts/",),
    "desktop": ("apps/desktop/",),
    "web": ("apps/web/",),
    "sdk": ("sdk/",),
    "native-macos": ("native/macos/",),
    "native-windows": ("native/windows/",),
    "migrations": ("migrations/",),
    "packs": ("packs/",),
    "infra": ("infra/",),
    "tests": ("tests/",),
    "config": ("config/",),
}

# Authority tables a non-Rust component must never mutate directly (DOSSIER.md \u00a75/\u00a73).
AUTHORITY_TABLES = (
    "work_graph",
    "agent_graph",
    "state_graph",
    "graph_transaction",
    "runtime_events",
    "protocol_state",
    "effect_ledger",
    "approval",
    "capability_projection",
    "audit_log",
    "tenant",
)

SQL_WRITE_RE = re.compile(
    r"\b(?:INSERT\s+INTO|UPDATE|DELETE\s+FROM|TRUNCATE|ALTER\s+TABLE|CREATE\s+TABLE)\s+"
    r"(?:public\.|control\.|runtime\.)?\"?(" + "|".join(AUTHORITY_TABLES) + r")",
    re.IGNORECASE,
)
PROVIDER_SDK_RE = re.compile(
    r"^\s*(?:import|from)\s+(anthropic|openai|google\.generativeai|vertexai|boto3|litellm|mistralai|cohere)\b",
    re.MULTILINE,
)
MODEL_ID_RE = re.compile(
    r"[\"'](claude-[a-z0-9.\-]+|gpt-[0-9][a-z0-9.\-]*|o[1-9]-[a-z]+|gemini-[0-9][a-z0-9.\-]*)[\"']"
)
TOOL_REGISTRATION_RE = re.compile(r"\b(register_tool|registerTool|registerToolHandler)\s*\(")
CLIENT_DB_RE = re.compile(
    r"(?:^|\n)\s*(?:import\b[^\n]*\bfrom\s+|import\s+|from\s+|const\s+\w+\s*=\s*require\s*\()"
    r"['\"]?(pg|knex|prisma|better-sqlite3|mysql2|sqlite3)['\"]?"
    r"(?=\s|['\"]|;|\)|$)",
    re.MULTILINE,
)

Rule = Tuple[str, str, str]  # rule_id, applies_to path prefix, description


def is_product_code(rel: str) -> bool:
    if rel in EXCLUDED_FILES:
        return False
    if rel.endswith(EXCLUDED_SUFFIXES):
        return False
    return not any(rel.startswith(p) for p in EXCLUDED_PREFIXES)


def owner_of(rel: str) -> Optional[str]:
    best: Optional[str] = None
    best_len = -1
    for owner, prefixes in CANONICAL_OWNERS.items():
        for p in prefixes:
            if rel.startswith(p) and len(p) > best_len:
                best, best_len = owner, len(p)
    return best


def tracked_files() -> List[str]:
    """Tracked plus untracked (non-ignored) files, or all files when not a repository.

    Untracked files are included so the scan covers in-progress code before it is
    committed.
    """
    def git_ls(args: Sequence[str]) -> Optional[List[str]]:
        try:
            out = subprocess.run(
                ["git", *args], cwd=str(ROOT), capture_output=True, text=True, check=True
            ).stdout
            return [l for l in out.splitlines() if l.strip()]
        except (subprocess.CalledProcessError, FileNotFoundError):
            return None

    tracked = git_ls(["ls-files"])
    if tracked is not None:
        untracked = git_ls(["ls-files", "--others", "--exclude-standard"]) or []
        if tracked or untracked:
            return sorted(set(tracked) | set(untracked))
    return sorted(
        str(p.relative_to(ROOT))
        for p in ROOT.rglob("*")
        if p.is_file() and ".git/" not in str(p)
    )


def git_info() -> Dict[str, object]:
    def run(*args: str) -> Optional[str]:
        try:
            return subprocess.run(
                ["git", *args], cwd=str(ROOT), capture_output=True, text=True, check=True
            ).stdout.strip()
        except (subprocess.CalledProcessError, FileNotFoundError):
            return None

    inside = run("rev-parse", "--is-inside-work-tree")
    if inside != "true":
        return {"is_repository": False, "branch": None, "head": None, "commits": 0}
    log = run("log", "--oneline") or ""
    return {
        "is_repository": True,
        "branch": run("branch", "--show-current"),
        "head": run("rev-parse", "HEAD"),
        "commits": len([l for l in log.splitlines() if l.strip()]),
        "dirty": bool((run("status", "--porcelain") or "").strip()),
    }


def inventory() -> Dict[str, object]:
    files = tracked_files()
    product = [
        f for f in files if is_product_code(f) and Path(f).suffix in LANG_BY_SUFFIX
    ]
    by_language: Dict[str, int] = {}
    by_owner: Dict[str, int] = {}
    unowned: List[str] = []
    for rel in product:
        lang = LANG_BY_SUFFIX.get(Path(rel).suffix, "other")
        by_language[lang] = by_language.get(lang, 0) + 1
        owner = owner_of(rel)
        if owner is None:
            unowned.append(rel)
        else:
            by_owner[owner] = by_owner.get(owner, 0) + 1

    def dirs(*patterns: str) -> List[str]:
        found: List[str] = []
        for pat in patterns:
            for p in ROOT.glob(pat):
                found.append(str(p.relative_to(ROOT)) + ("/" if p.is_dir() else ""))
        return sorted(found)

    inventory_dirs = {
        "crates": dirs("crates/*"),
        "python": dirs("python/*"),
        "apps": dirs("apps/*"),
        "native": dirs("native/*"),
        "schemas": dirs("schemas/*"),
        "migrations": dirs("migrations/*"),
        "packs": dirs("packs/*"),
        "sdk": dirs("sdk/*"),
        "config": dirs("config/*"),
        "infra": dirs("infra/*"),
        "tests": dirs("tests/*"),
        "ci_workflows": dirs(".github/workflows/*.yml", ".github/workflows/*.yaml"),
    }
    return {
        "authority_files": [f for f in AUTHORITY_FILES if (ROOT / f).exists()],
        "authority_files_missing": [f for f in AUTHORITY_FILES if not (ROOT / f).exists()],
        "git": git_info(),
        "tracked_files": len(files),
        "product_code_files": len(product),
        "product_code_by_language": dict(sorted(by_language.items())),
        "product_code_by_owner": dict(sorted(by_owner.items())),
        "files_without_canonical_owner": sorted(unowned),
        "canonical_owner_dirs_present": inventory_dirs,
        "deployables": dirs("infra/compose/*", "infra/helm/*", "infra/terraform/*")
        + dirs("Dockerfile", "crates/*/Dockerfile"),
    }


FUZZY_LANG = {
    ".rs": "rust",
    ".ts": "typescript",
    ".tsx": "typescript",
    ".swift": "swift",
    ".py": "python",
    ".go": "go",
}


def scan(
    files: Optional[Sequence[str]] = None,
    root: Optional[Path] = None,
    include_tests: bool = False,
) -> List[Dict[str, object]]:
    """Duplicate-authority / language-boundary scan over product code.

    `files` are repository-relative paths; `root` is the tree they live in
    (defaults to the repository root). Returns one finding per violation. An
    empty list means the tree has no competing authority path.
    """
    root = Path(root) if root is not None else ROOT
    if files is None:
        files = [
            f
            for f in tracked_files()
            if is_product_code(f) and (include_tests or not f.startswith("tests/"))
        ]
    findings: List[Dict[str, object]] = []

    def add(rule: str, rel: str, line: int, detail: str, snippet: str) -> None:
        findings.append(
            {
                "rule": rule,
                "path": rel,
                "line": line,
                "detail": detail,
                "snippet": snippet.strip()[:160],
            }
        )

    for rel in files:
        suffix = Path(rel).suffix
        if suffix not in LANG_BY_SUFFIX:
            continue
        try:
            text = (root / rel).read_text(errors="replace")
        except (OSError, UnicodeDecodeError):
            continue
        lines = text.splitlines()
        owner = owner_of(rel)
        lang = FUZZY_LANG.get(suffix)

        if owner is None:
            add("no-canonical-owner", rel, 1, "product file does not map to a canonical V8.1 owner", "")

        if lang in {"python", "typescript", "javascript"}:
            for m in SQL_WRITE_RE.finditer(text):
                line = text[: m.start()].count("\n") + 1
                add(
                    "non-rust-authority-write",
                    rel,
                    line,
                    f"{lang} writes canonical authority table '{m.group(1)}' (DOSSIER.md \u00a73)",
                    lines[line - 1] if line - 1 < len(lines) else "",
                )

        if lang == "python" and "python/intelligence/model_gateway/" not in rel:
            m = PROVIDER_SDK_RE.search(text)
            if m:
                line = text[: m.start()].count("\n") + 1
                add(
                    "provider-sdk-outside-gateway",
                    rel,
                    line,
                    f"provider SDK '{m.group(1)}' imported outside the model gateway (D-006)",
                    lines[line - 1] if line - 1 < len(lines) else "",
                )

        if rel.startswith(("apps/", "sdk/")) and lang in {"typescript", "javascript"}:
            m = CLIENT_DB_RE.search(text)
            if m:
                line = text[: m.start()].count("\n") + 1
                add(
                    "client-direct-database",
                    rel,
                    line,
                    f"client imports '{m.group(1)}' (clients are never persistence authority, DOSSIER.md \u00a74.1)",
                    lines[line - 1] if line - 1 < len(lines) else "",
                )

        if lang in {"python", "typescript", "javascript"}:
            m = TOOL_REGISTRATION_RE.search(text)
            if m:
                line = text[: m.start()].count("\n") + 1
                add(
                    "tool-registry-outside-rust",
                    rel,
                    line,
                    "tool registration outside the Rust Tool Registry (RUN-011/D-014)",
                    lines[line - 1] if line - 1 < len(lines) else "",
                )

        if lang in {"python", "typescript", "javascript", "rust"} and not rel.startswith("config/"):
            for m in MODEL_ID_RE.finditer(text):
                line = text[: m.start()].count("\n") + 1
                add(
                    "hardcoded-model-id",
                    rel,
                    line,
                    f"model identifier '{m.group(1)}' outside config/ (D-018)",
                    lines[line - 1] if line - 1 < len(lines) else "",
                )

        if suffix == ".go":
            add("new-go-code", rel, 1, "Go is not a V8.1 development language (D-011)", "")
    return findings


def render_inventory(inv: Dict[str, object]) -> str:
    out: List[str] = ["# Quansio V8.1 repository inventory", ""]
    git = inv["git"]  # type: ignore[index]
    out.append(f"git repository: {git['is_repository']}  branch: {git['branch']}  head: {git['head']}")
    out.append(f"tracked files: {inv['tracked_files']}  product code files: {inv['product_code_files']}")
    out.append(f"authority files present: {len(inv['authority_files'])}/{len(AUTHORITY_FILES)}")
    if inv["authority_files_missing"]:
        out.append(f"authority files MISSING: {', '.join(inv['authority_files_missing'])}")
    out.append("")
    out.append("product code by language: " + (json.dumps(inv["product_code_by_language"]) or "{}"))
    out.append("product code by canonical owner: " + (json.dumps(inv["product_code_by_owner"]) or "{}"))
    if inv["files_without_canonical_owner"]:
        out.append("files without canonical owner: " + ", ".join(inv["files_without_canonical_owner"]))
    else:
        out.append("files without canonical owner: none")
    out.append("")
    out.append("present component directories:")
    for group, entries in inv["canonical_owner_dirs_present"].items():  # type: ignore[union-attr]
        out.append(f"  {group}: " + (", ".join(entries) if entries else "(none)"))
    out.append("")
    out.append("deployables: " + (", ".join(inv["deployables"]) if inv["deployables"] else "(none)"))
    return "\n".join(out)


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Repository inventory and duplicate-authority scan")
    ap.add_argument("--json", action="store_true", help="emit the inventory as JSON")
    ap.add_argument("--scan", action="store_true", help="run the duplicate-authority scan")
    ap.add_argument("--scan-json", action="store_true", help="run the scan and emit findings as JSON")
    args = ap.parse_args(argv)

    if args.scan or args.scan_json:
        findings = scan()
        if args.scan_json:
            print(json.dumps(findings, indent=2))
        else:
            if not findings:
                print("duplicate-authority scan: CLEAN (0 findings)")
            for f in findings:
                print(f"{f['rule']}: {f['path']}:{f['line']}: {f['detail']} :: {f['snippet']}")
            if findings:
                print(f"duplicate-authority scan: {len(findings)} finding(s)")
        return 1 if findings else 0

    inv = inventory()
    print(json.dumps(inv, indent=2) if args.json else render_inventory(inv))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
