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
import os
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
    # Routed through the pinned venv (unlike the standard-library-only gates above):
    # its YAML manifest scan needs PyYAML, which is a python/ project dependency, not
    # a guarantee about whatever `python3.12` happens to resolve to on PATH. A bare
    # interpreter without PyYAML makes every manifest "unparsable" and the gate fails
    # closed -- not a code defect, but a real pipeline-correctness bug: the same
    # commit reports FAIL or PASS depending on the *host's* ambient site-packages,
    # which is exactly what DOSSIER.md section 18's pinned-environment policy exists
    # to prevent. Reproduced and fixed 2026-09-16 (evidence/OPS-007/).
    ("supply-chain", "uv run --project python python scripts/ci/supply_chain/check.py"),
    ("legacy-map", "python3.12 scripts/ci/legacy_map_check.py"),
    ("contract-drift", "uv run --project python python scripts/ci/gen_contracts.py --check"),
    ("contract-lint-compat", "uv run --project python python scripts/ci/contract_compat.py"),
    ("toolchains", "bash scripts/dev/bootstrap.sh"),
    ("tests", "uv run --project python pytest tests -q"),
]
# `arch_check.py` is created by GOV-008; until it exists the gate is skipped explicitly
# (never silently passed) and reported as `SKIPPED_PENDING` in the summary.
OPTIONAL_GATES = {"architecture": "scripts/ci/arch_check.py"}


def dev_database_url(root: Path = ROOT) -> Optional[str]:
    """Database URL for database-backed tests, derived from the generated dev `.env`.

    `scripts/dev/up` writes the gitignored `.env`; when it exists the baseline pipeline
    exports `QUANSIO_TEST_POSTGRES_URL` so schema tests exercise a real database instead
    of reporting `BLOCKED_EXTERNAL`. An explicit environment value always wins.
    """
    if os.environ.get("QUANSIO_TEST_POSTGRES_URL"):
        return os.environ["QUANSIO_TEST_POSTGRES_URL"]
    env_file = root / ".env"
    if not env_file.exists():
        return None
    values: Dict[str, str] = {}
    for line in env_file.read_text().splitlines():
        if "=" in line and not line.strip().startswith("#"):
            key, _, value = line.partition("=")
            values[key.strip()] = value.strip()
    host = values.get("QUANSIO_DEV_POSTGRES_HOST", "127.0.0.1")
    port = values.get("QUANSIO_DEV_POSTGRES_PORT", "55440")
    user = values.get("QUANSIO_DEV_POSTGRES_USER", "quansio")
    password = values.get("QUANSIO_DEV_POSTGRES_PASSWORD")
    database = values.get("QUANSIO_DEV_POSTGRES_DB", "quansio")
    if not password:
        return None
    return f"postgres://{user}:{password}@{host}:{port}/{database}"


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


def run_gate(
    name: str,
    command: str,
    root: Path,
    timeout: int = 3600,
    env: Optional[Dict[str, str]] = None,
    log_dir: Optional[Path] = None,
) -> Dict[str, object]:
    started = time.monotonic()
    full_output: Optional[str] = None
    try:
        result = subprocess.run(
            command,
            cwd=str(root),
            shell=True,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
        exit_code: Optional[int] = result.returncode
        full_output = result.stdout + result.stderr
        tail = "\n".join(full_output.splitlines()[-25:])
    except subprocess.TimeoutExpired:
        exit_code = None
        tail = f"gate timed out after {timeout}s"
    duration = round(time.monotonic() - started, 3)
    status = "PASS" if exit_code == 0 else ("TIMEOUT" if exit_code is None else "FAIL")
    entry: Dict[str, object] = {
        "gate": name,
        "command": command,
        "status": status,
        "exit_code": exit_code,
        "duration_s": duration,
        "tail": tail,
    }
    # The summary's own `tail` is 25 lines -- enough for an "it's red" glance, not
    # enough to diagnose most real failures (a single verbose gate, e.g. `cargo test
    # --workspace`, prints far more than that before the actual failure). Every
    # gate's complete stdout+stderr is written alongside it so a red run is
    # diagnosable from the uploaded artifact alone, with no need to reproduce it
    # (or, worse, guess at it) to find out what actually happened.
    if log_dir is not None and full_output is not None:
        log_dir.mkdir(parents=True, exist_ok=True)
        log_path = log_dir / f"{name}.log"
        log_path.write_text(full_output)
        try:
            entry["log"] = str(log_path.relative_to(root))
        except ValueError:
            entry["log"] = str(log_path)
    return entry


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
    env = os.environ.copy()
    database_url = dev_database_url(root)
    if database_url:
        env["QUANSIO_TEST_POSTGRES_URL"] = database_url
    commit = git_commit(root) or "nogit"
    log_dir = (out.parent if out else DEFAULT_OUT_DIR) / "logs" / commit[:12]

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
        results.append(run_gate(name, command, root, env=env, log_dir=log_dir))

    artifacts: Dict[str, str] = {}
    for relative in ("registries/progress.json", "TASKS.md", "MANIFEST.json"):
        path = root / relative
        if path.exists():
            artifacts[relative] = sha256_file(path)

    summary = {
        "commit": commit if commit != "nogit" else None,
        "recorded_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "results": results,
        "artifacts": artifacts,
        "status": "FAIL" if any(r["status"] == "FAIL" or r["status"] == "TIMEOUT" for r in results) else "PASS",
        "database_backed_tests": database_url is not None,
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
