#!/usr/bin/env python3
"""Architecture conformance gate (GOV-008).

One deterministic gate over the tree that stops shadow runtimes, stores and
privileged shortcuts (DOSSIER.md \u00a71, \u00a73, \u00a75, \u00a716). It reuses the duplicate-authority
rule engine in `scripts/ci/inventory.py` (imports it and calls `inventory.scan`)
instead of duplicating it, and adds the forbidden-wiring rules below. Any finding
makes the exit code non-zero and is printed as `rule: path:line: detail`.

Rules owned here:

  renderer-to-provider             TypeScript under `apps/` (and anything under
                                   `apps/*/renderer/`) may not import a model-provider
                                   SDK or name a provider base-URL host.
  python-authority-write           Python may not write canonical authority tables
                                   (from the inventory engine) or import a generated
                                   Rust-authority RPC client other than
                                   `quansio.v1.intelligence.*`.
  worker-to-control-db             `crates/qworkerd/` and `native/` may not reference
                                   control-plane tables, a control database URL env var
                                   or a Postgres client: qworkerd talks to the server
                                   only over the fenced worker channel.
  gateway-effect-execution         `python/intelligence/model_gateway/` may not import
                                   effect/settlement/approval machinery or register tools.
  tool-registration-outside-owner  only `crates/tools/` registers tools (Rust
                                   `#[tool(...)]` / `.register_tool(`); the inventory
                                   engine covers the non-Rust registration calls.
  second-scheduler-or-timer        timer/scheduler loops belong to
                                   `crates/server/src/scheduler/` and `crates/events/`.
  new-deployable-without-decision  a deployable (Dockerfile, compose file/service,
                                   helm chart, terraform file) that is not in the
                                   allowlist anchored to `infra/` needs a matching
                                   `D-###` decision in `DOSSIER.md`.
  new-persistent-store             a runtime dependency on a database/queue client
                                   declared outside the canonical store owners
                                   (crates/server, crates/events, crates/graph,
                                   crates/machine, crates/indexer,
                                   python/intelligence/embeddings). Only runtime
                                   dependency tables are inspected (Cargo
                                   `[dependencies]`, package.json `dependencies`/
                                   `peerDependencies`, pyproject
                                   `[project].dependencies`, `requirements*.txt`);
                                   dev/test dependency groups are tooling, not a
                                   product store path.

Every rule adds findings with the same shape as the inventory engine
(`rule`, `path`, `line`, `detail`, `snippet`), so `--json` is machine-readable.

`second-scheduler-or-timer` matches exactly these patterns, nothing else:

  * `tokio::time::interval(` and `tokio::time::interval_at(`
  * `setInterval(`
  * `schedule_interval`
  * `croniter`
  * `tokio::spawn(` followed by `loop {` within 400 characters (a spawned loop)

The list is deliberately narrow: a one-shot `tokio::spawn` without a loop, a unit
test that merely names a timer, or a normal polling helper does not trip it.

  python3 scripts/ci/arch_check.py                 gate the repository
  python3 scripts/ci/arch_check.py --json          findings as JSON
  python3 scripts/ci/arch_check.py --root <dir>    gate a fixture tree

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
    sys.stderr.write("arch_check.py requires Python 3.11+ (run it with python3.12)\n")
    raise SystemExit(2)

from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:  # runnable as a script and importable as a module
    sys.path.insert(0, str(ROOT))

from scripts.ci import inventory  # noqa: E402

Finding = Dict[str, Any]

# --- renderer-to-provider ---------------------------------------------------------
RENDERER_DIR_RE = re.compile(r"^apps/[^/]+/renderer/")
PROVIDER_TS_IMPORT_RE = re.compile(
    r"""(?:from|import|require)\s*\(?\s*["']"""
    r"""(?P<pkg>anthropic|openai|@anthropic-ai/[^"']*|@google/generative-ai|google\.generativeai)["']"""
)
PROVIDER_HOST_RE = re.compile(r"api\.(?:anthropic|openai)\.com")

# --- python-authority-write -------------------------------------------------------
PY_RPC_IMPORT_RE = re.compile(r"^\s*(?:from|import)\s+(quansio\.v1\.[A-Za-z0-9_.]+)", re.MULTILINE)
# Generated RPC modules Python may import: the intelligence plane's own service, plus
# read-only service stubs as they are generated (none exist yet).
PY_RPC_ALLOWED_PREFIXES = ("quansio.v1.intelligence",)
PY_GENERATED_PREFIX = "python/intelligence/contracts/generated/"

# --- worker-to-control-db ---------------------------------------------------------
WORKER_DB_PREFIXES = ("crates/qworkerd/", "native/")
CONTROL_TABLE_RE = re.compile(
    r"\b(runtime_events|protocol_state|effect_ledger|work_graph|agent_graph|capability_projection)\b"
)
# `approval` is a common prose word, so it is only a table reference in a quoted
# identifier or an SQL clause.
APPROVAL_TABLE_RE = re.compile(
    r"""["'`]approvals?["'`]|\b(?:FROM|INTO|UPDATE|JOIN|TABLE)\s+"?approvals?\b""",
    re.IGNORECASE,
)
CONTROL_DB_ENV_RE = re.compile(
    r"\b(?:DATABASE_URL|POSTGRES(?:QL)?_URL|QUANSIO_[A-Z0-9_]*(?:DB|DATABASE)[A-Z0-9_]*)\b"
)
CONTROL_DB_CLIENT_RE = re.compile(
    r"(?:sqlx::(?:PgPool|PgConnection|Postgres|postgres)|tokio_postgres|postgres::|diesel::"
    r"|use\s+sqlx\b|\bPgPool\b|\bPgConnection\b)"
)

# --- gateway-effect-execution -----------------------------------------------------
GATEWAY_PREFIX = "python/intelligence/model_gateway/"
GATEWAY_EFFECT_IMPORT_RE = re.compile(
    r"^\s*(?:from|import)\b[^\n]*\b(effect_ledger|reserve_effect|ApprovalReceipt|settle\w*"
    r"|quansio\.v1\.effects|quansio\.v1\.trust|tool_registry)\b",
    re.MULTILINE,
)
TOOL_REGISTER_CALL_RE = re.compile(r"\b(register_tool|registerTool|registerToolHandler)\s*\(")

# --- tool-registration-outside-owner ----------------------------------------------
TOOLS_OWNER_PREFIX = "crates/tools/"
RUST_TOOL_ATTR_RE = re.compile(r"#\[\s*tool\s*\(")
RUST_TOOL_CALL_RE = re.compile(r"\.register_tool\s*\(")

# --- second-scheduler-or-timer ----------------------------------------------------
SCHEDULER_OWNER_PREFIXES = ("crates/server/src/scheduler/", "crates/events/")
SCHEDULER_PATTERNS: Tuple[Tuple[re.Pattern, str], ...] = (
    (re.compile(r"tokio::time::interval(?:_at)?\s*\("), "tokio::time::interval loop"),
    (re.compile(r"\bsetInterval\s*\("), "setInterval timer"),
    (re.compile(r"\bschedule_interval\b"), "schedule_interval timer"),
    (re.compile(r"\bcroniter\b"), "croniter schedule"),
    (
        re.compile(r"tokio::spawn\s*\([^;]{0,400}?\bloop\s*\{", re.DOTALL),
        "tokio::spawn loop",
    ),
)
SCHEDULER_SUFFIXES = (".rs", ".py", ".ts", ".tsx", ".js", ".mjs", ".swift")

# --- new-deployable-without-decision ----------------------------------------------
DEPLOYABLE_FILENAMES = frozenset(
    {
        "Dockerfile",
        "Containerfile",
        "Chart.yaml",
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    }
)
DEPLOYABLE_SUFFIXES = (".dockerfile", ".tf")
COMPOSE_FILENAMES = frozenset(
    {"compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"}
)
# Canonical deployables (DOSSIER.md \u00a74): the allowlist is anchored to infra/.
ALLOWLISTED_DEPLOYABLES = ("infra/compose/compose.yaml",)
ALLOWLISTED_COMPOSE_SERVICES = {
    "infra/compose/compose.yaml": frozenset(
        {"postgres", "nats", "minio", "minio-init", "stub-provider", "vector-adapter"}
    )
}
COMPOSE_SERVICE_RE = re.compile(r"^  ([A-Za-z0-9][A-Za-z0-9_.-]*)\s*:\s*$")
DECISION_RE = re.compile(r"^- \*\*(D-\d{3}):\*\*", re.MULTILINE)

# --- new-persistent-store ---------------------------------------------------------
# Modules that own authoritative tables (or the derived vector index) in the canonical
# schema and may therefore hold a database client. A module outside this list with a
# store client is a *new* persistent store and fails the gate. `crates/capability` owns
# `capability_projections`, so it belongs here even though its writes go through the
# canonical event-emitting transaction (`EventStore::commit_mutation*`) rather than a
# private connection.
STORE_OWNER_PREFIXES = (
    "crates/server/",
    "crates/events/",
    "crates/graph/",
    "crates/capability/",
    "crates/machine/",
    "crates/indexer/",
    "python/intelligence/embeddings/",
)
CARGO_STORE_DEPS = frozenset({"sqlx", "diesel", "redis", "nats", "rusqlite", "mongodb", "kafka"})
NPM_STORE_DEPS = frozenset(
    {"pg", "pg-pool", "pg-promise", "ioredis", "redis", "nats", "mongodb", "better-sqlite3", "kafkajs"}
)
PY_STORE_DEPS = frozenset(
    {
        "psycopg",
        "psycopg2",
        "psycopg-pool",
        "asyncpg",
        "sqlalchemy",
        "redis",
        "nats-py",
        "pymongo",
        "motor",
        "kafka-python",
        "aiokafka",
        "aiosqlite",
    }
)
PY_REQUIREMENT_NAME_RE = re.compile(r"^\s*([A-Za-z0-9][A-Za-z0-9._-]*)")

# Inventory rule ids that this gate presents under its own rule vocabulary.
RULE_ALIASES = {
    "non-rust-authority-write": "python-authority-write",
    "tool-registry-outside-rust": "tool-registration-outside-owner",
}


def _rel_files(root: Path) -> List[str]:
    """Repository-relative files (git-tracked for the real repository, else a walk)."""
    if root == ROOT:
        return inventory.tracked_files()
    return sorted(
        str(p.relative_to(root)) for p in root.rglob("*") if p.is_file() and ".git" not in p.parts
    )


def _read(root: Path, rel: str) -> str:
    try:
        return (root / rel).read_text(errors="replace")
    except OSError:
        return ""


def _line_of(text: str, index: int) -> int:
    return text[:index].count("\n") + 1


def _snippet(lines: Sequence[str], line: int) -> str:
    return lines[line - 1].strip()[:160] if 0 < line <= len(lines) else ""


def _alias(finding: Finding) -> Finding:
    """Present inventory findings under this gate's rule vocabulary."""
    rule = str(finding["rule"])
    aliased = rule == "tool-registry-outside-rust" or (
        rule == "non-rust-authority-write" and str(finding["path"]).endswith(".py")
    )
    return {**finding, "rule": RULE_ALIASES[rule]} if aliased else finding


def _inventory_findings(root: Path, code_files: Sequence[str]) -> List[Finding]:
    """Findings from the shared inventory rule engine, re-labelled."""
    return [_alias(f) for f in inventory.scan(files=list(code_files), root=root)]


def _renderer_to_provider(root: Path, code_files: Sequence[str]) -> List[Finding]:
    findings: List[Finding] = []
    for rel in code_files:
        if not (rel.endswith((".ts", ".tsx")) or RENDERER_DIR_RE.match(rel)):
            continue
        text = _read(root, rel)
        if not text:
            continue
        lines = text.splitlines()
        for match in PROVIDER_TS_IMPORT_RE.finditer(text):
            line = _line_of(text, match.start())
            findings.append(
                {
                    "rule": "renderer-to-provider",
                    "path": rel,
                    "line": line,
                    "detail": (
                        f"renderer imports model-provider SDK '{match.group('pkg')}' "
                        "(D-006: model fulfillment is gateway-mediated)"
                    ),
                    "snippet": _snippet(lines, line),
                }
            )
        for match in PROVIDER_HOST_RE.finditer(text):
            line = _line_of(text, match.start())
            findings.append(
                {
                    "rule": "renderer-to-provider",
                    "path": rel,
                    "line": line,
                    "detail": (
                        f"renderer references provider base-URL host '{match.group(0)}' "
                        "(D-006: provider credentials never reach the renderer)"
                    ),
                    "snippet": _snippet(lines, line),
                }
            )
    return findings


def _python_authority_write(root: Path, code_files: Sequence[str]) -> List[Finding]:
    """Python calling a generated Rust-authority RPC client that is not the intelligence plane."""
    findings: List[Finding] = []
    for rel in code_files:
        if not (rel.startswith("python/") and rel.endswith(".py")):
            continue
        if rel.startswith(PY_GENERATED_PREFIX):
            continue
        text = _read(root, rel)
        if not text:
            continue
        lines = text.splitlines()
        for match in PY_RPC_IMPORT_RE.finditer(text):
            module = match.group(1)
            if module.startswith(PY_RPC_ALLOWED_PREFIXES):
                continue
            line = _line_of(text, match.start())
            findings.append(
                {
                    "rule": "python-authority-write",
                    "path": rel,
                    "line": line,
                    "detail": (
                        f"Python imports generated Rust-authority client '{module}' "
                        "(D-002: Python mutates canonical state only through proposals)"
                    ),
                    "snippet": _snippet(lines, line),
                }
            )
    return findings


def _worker_to_control_db(root: Path, code_files: Sequence[str]) -> List[Finding]:
    findings: List[Finding] = []
    for rel in code_files:
        if not rel.startswith(WORKER_DB_PREFIXES):
            continue
        text = _read(root, rel)
        if not text:
            continue
        lines = text.splitlines()
        patterns: Tuple[Tuple[re.Pattern, str], ...] = (
            (CONTROL_TABLE_RE, "references control-plane table"),
            (APPROVAL_TABLE_RE, "references control-plane table"),
            (CONTROL_DB_ENV_RE, "references a control database URL environment variable"),
            (CONTROL_DB_CLIENT_RE, "uses a Postgres client"),
        )
        for pattern, detail in patterns:
            for match in pattern.finditer(text):
                line = _line_of(text, match.start())
                findings.append(
                    {
                        "rule": "worker-to-control-db",
                        "path": rel,
                        "line": line,
                        "detail": (
                            f"{detail} '{match.group(0)}': qworkerd/native reach the server only "
                            "over the fenced worker channel"
                        ),
                        "snippet": _snippet(lines, line),
                    }
                )
    return findings


def _gateway_effect_execution(root: Path, code_files: Sequence[str]) -> List[Finding]:
    findings: List[Finding] = []
    for rel in code_files:
        if not rel.startswith(GATEWAY_PREFIX):
            continue
        text = _read(root, rel)
        if not text:
            continue
        lines = text.splitlines()
        for match in GATEWAY_EFFECT_IMPORT_RE.finditer(text):
            line = _line_of(text, match.start())
            findings.append(
                {
                    "rule": "gateway-effect-execution",
                    "path": rel,
                    "line": line,
                    "detail": (
                        f"model gateway imports effect/approval machinery '{match.group(1)}' "
                        "(the gateway proposes; the runtime settles)"
                    ),
                    "snippet": _snippet(lines, line),
                }
            )
        for match in TOOL_REGISTER_CALL_RE.finditer(text):
            line = _line_of(text, match.start())
            findings.append(
                {
                    "rule": "gateway-effect-execution",
                    "path": rel,
                    "line": line,
                    "detail": (
                        f"model gateway registers tool '{match.group(1)}' "
                        "(the Tool Registry lives in crates/tools)"
                    ),
                    "snippet": _snippet(lines, line),
                }
            )
    return findings


def _tool_registration_rust(root: Path, code_files: Sequence[str]) -> List[Finding]:
    findings: List[Finding] = []
    for rel in code_files:
        if not rel.endswith(".rs") or rel.startswith(TOOLS_OWNER_PREFIX):
            continue
        text = _read(root, rel)
        if not text:
            continue
        lines = text.splitlines()
        patterns: Tuple[Tuple[re.Pattern, str], ...] = (
            (RUST_TOOL_ATTR_RE, "#[tool(...)] registration"),
            (RUST_TOOL_CALL_RE, ".register_tool(...) registration"),
        )
        for pattern, detail in patterns:
            for match in pattern.finditer(text):
                line = _line_of(text, match.start())
                findings.append(
                    {
                        "rule": "tool-registration-outside-owner",
                        "path": rel,
                        "line": line,
                        "detail": (
                            f"{detail} outside crates/tools/ "
                            "(RUN-011/D-014: one Tool Registry, owned by crates/tools)"
                        ),
                        "snippet": _snippet(lines, line),
                    }
                )
    return findings


def _second_scheduler_or_timer(root: Path, code_files: Sequence[str]) -> List[Finding]:
    findings: List[Finding] = []
    for rel in code_files:
        if rel.startswith(SCHEDULER_OWNER_PREFIXES) or not rel.endswith(SCHEDULER_SUFFIXES):
            continue
        text = _read(root, rel)
        if not text:
            continue
        lines = text.splitlines()
        for pattern, detail in SCHEDULER_PATTERNS:
            for match in pattern.finditer(text):
                line = _line_of(text, match.start())
                findings.append(
                    {
                        "rule": "second-scheduler-or-timer",
                        "path": rel,
                        "line": line,
                        "detail": (
                            f"{detail} outside the canonical scheduler "
                            "(CORE-008: crates/server/src/scheduler and crates/events own timers)"
                        ),
                        "snippet": _snippet(lines, line),
                    }
                )
    return findings


def _decision_references(root: Path, *needles: str) -> bool:
    """True when one `D-###` decision entry in DOSSIER.md names every needle."""
    dossier = root / "DOSSIER.md"
    if not dossier.exists():
        return False
    text = dossier.read_text(errors="replace")
    starts = [m.start() for m in DECISION_RE.finditer(text)]
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else len(text)
        block = text[start:end]
        if all(needle in block for needle in needles):
            return True
    return False


def _compose_services(text: str) -> List[Tuple[str, int]]:
    """(service name, line) pairs from the top-level `services:` mapping."""
    services: List[Tuple[str, int]] = []
    in_services = False
    for number, line in enumerate(text.splitlines(), start=1):
        if re.match(r"^services:\s*$", line):
            in_services = True
            continue
        if not in_services:
            continue
        if line.strip() and not line.startswith((" ", "\t")):
            break
        match = COMPOSE_SERVICE_RE.match(line)
        if match:
            services.append((match.group(1), number))
    return services


def _new_deployable_without_decision(root: Path, files: Sequence[str]) -> List[Finding]:
    findings: List[Finding] = []
    for rel in files:
        name = Path(rel).name
        if name not in DEPLOYABLE_FILENAMES and not rel.endswith(DEPLOYABLE_SUFFIXES):
            continue
        if rel not in ALLOWLISTED_DEPLOYABLES and not _decision_references(root, rel):
            findings.append(
                {
                    "rule": "new-deployable-without-decision",
                    "path": rel,
                    "line": 1,
                    "detail": (
                        "new deployable is not in the infra/ allowlist and no D-### decision "
                        "in DOSSIER.md references it (DOSSIER.md \u00a718)"
                    ),
                    "snippet": "",
                }
            )
        if name not in COMPOSE_FILENAMES or rel not in ALLOWLISTED_COMPOSE_SERVICES:
            continue
        allowed = ALLOWLISTED_COMPOSE_SERVICES[rel]
        text = _read(root, rel)
        for service, line in _compose_services(text):
            if service in allowed or _decision_references(root, rel, service):
                continue
            findings.append(
                {
                    "rule": "new-deployable-without-decision",
                    "path": rel,
                    "line": line,
                    "detail": (
                        f"compose service '{service}' is not in the infra/ allowlist and no "
                        "D-### decision in DOSSIER.md references it (DOSSIER.md \u00a74/\u00a718)"
                    ),
                    "snippet": _snippet(text.splitlines(), line),
                }
            )
    return findings


def _dependency_line(text: str, dependency: str) -> int:
    pattern = re.compile(r'^\s*"?' + re.escape(dependency) + r'"?\s*[:=]', re.MULTILINE)
    match = pattern.search(text)
    if match is None:
        match = re.search(r"(?<![\w.-])" + re.escape(dependency) + r"(?![\w.-])", text)
    return _line_of(text, match.start()) if match else 1


def _cargo_runtime_dependencies(text: str) -> List[str]:
    """Runtime dependency names from a Cargo manifest (dev/build deps are tooling)."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return []
    names = set((data.get("dependencies") or {}).keys())
    for target in (data.get("target") or {}).values():
        names.update(((target or {}).get("dependencies") or {}).keys())
    return sorted(names)


def _npm_runtime_dependencies(text: str) -> List[str]:
    try:
        data = json.loads(text)
    except json.JSONDecodeError:
        return []
    if not isinstance(data, dict):
        return []
    names = set((data.get("dependencies") or {}).keys())
    names.update((data.get("peerDependencies") or {}).keys())
    return sorted(names)


def _python_runtime_dependencies(text: str) -> List[str]:
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return []
    requirements = (data.get("project") or {}).get("dependencies") or []
    names: List[str] = []
    for requirement in requirements:
        match = PY_REQUIREMENT_NAME_RE.match(str(requirement))
        if match:
            names.append(match.group(1))
    return names


def _requirements_dependencies(text: str) -> List[str]:
    names: List[str] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(("#", "-")):
            continue
        match = PY_REQUIREMENT_NAME_RE.match(stripped)
        if match:
            names.append(match.group(1))
    return names


def _python_declared_packages(text: str) -> List[str]:
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return []
    wheel = (
        (data.get("tool") or {})
        .get("hatch", {})
        .get("build", {})
        .get("targets", {})
        .get("wheel", {})
    )
    return [f"python/{package}/" for package in (wheel.get("packages") or [])]


def _in_store_owner(rel: str, packages: Sequence[str]) -> bool:
    if rel.startswith(STORE_OWNER_PREFIXES):
        return True
    # The Python intelligence plane ships as one distribution whose declared
    # package scope contains the canonical embeddings owner.
    return any(
        owner.startswith(package) for package in packages for owner in STORE_OWNER_PREFIXES
    )


def _new_persistent_store(root: Path, files: Sequence[str]) -> List[Finding]:
    findings: List[Finding] = []
    for rel in files:
        name = Path(rel).name
        text = _read(root, rel)
        if not text:
            continue
        if name == "Cargo.toml":
            kind, dependencies, store_deps = "Rust", _cargo_runtime_dependencies(text), CARGO_STORE_DEPS
        elif name == "package.json":
            kind, dependencies, store_deps = "TypeScript", _npm_runtime_dependencies(text), NPM_STORE_DEPS
        elif name == "pyproject.toml":
            kind, dependencies, store_deps = "Python", _python_runtime_dependencies(text), PY_STORE_DEPS
        elif name.startswith("requirements") and name.endswith(".txt"):
            kind, dependencies, store_deps = "Python", _requirements_dependencies(text), PY_STORE_DEPS
        else:
            continue
        offenders = sorted(d for d in dependencies if d.lower() in store_deps)
        if not offenders:
            continue
        packages = _python_declared_packages(text) if name == "pyproject.toml" else []
        if _in_store_owner(rel, packages):
            continue
        for dependency in offenders:
            findings.append(
                {
                    "rule": "new-persistent-store",
                    "path": rel,
                    "line": _dependency_line(text, dependency),
                    "detail": (
                        f"{kind} runtime dependency '{dependency}' is a new store/queue client "
                        "declared outside the canonical store owners (DOSSIER.md \u00a75/\u00a79)"
                    ),
                    "snippet": _snippet(text.splitlines(), _dependency_line(text, dependency)),
                }
            )
    return findings


def check(root: Optional[Path] = None, include_tests: bool = False) -> List[Finding]:
    """All architecture findings for `root`; an empty list means conformance."""
    root = Path(root) if root is not None else ROOT
    files = _rel_files(root)
    code_files = [
        rel
        for rel in files
        if inventory.is_product_code(rel) and (include_tests or not rel.startswith("tests/"))
    ]
    findings: List[Finding] = []
    findings.extend(_inventory_findings(root, code_files))
    findings.extend(_renderer_to_provider(root, code_files))
    findings.extend(_python_authority_write(root, code_files))
    findings.extend(_worker_to_control_db(root, code_files))
    findings.extend(_gateway_effect_execution(root, code_files))
    findings.extend(_tool_registration_rust(root, code_files))
    findings.extend(_second_scheduler_or_timer(root, code_files))
    findings.extend(_new_deployable_without_decision(root, files))
    findings.extend(_new_persistent_store(root, files))
    return sorted(findings, key=lambda f: (str(f["path"]), int(f["line"]), str(f["rule"])))


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Architecture conformance gate (GOV-008)")
    ap.add_argument("--json", action="store_true", help="emit findings as JSON")
    ap.add_argument("--root", default=None, help="tree to gate (defaults to the repository root)")
    args = ap.parse_args(argv)

    findings = check(Path(args.root) if args.root else ROOT)
    if args.json:
        print(json.dumps(findings, indent=2))
    elif not findings:
        print("architecture check: CLEAN (0 findings)")
    else:
        for finding in findings:
            print(f"{finding['rule']}: {finding['path']}:{finding['line']}: {finding['detail']}")
        print(f"architecture check: {len(findings)} finding(s)")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
