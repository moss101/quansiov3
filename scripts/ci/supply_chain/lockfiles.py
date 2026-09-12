"""Lockfile pinning rules for the supply-chain gate (OPS-007).

Proves that every dependency declared by a manifest is resolved and pinned by the
lockfile that manifest feeds: `Cargo.toml` -> `Cargo.lock`, `package.json` ->
`pnpm-lock.yaml`, `python/pyproject.toml` -> `python/uv.lock`. A manifest that
declares a dependency no lockfile contains, a wildcard/floating requirement, a
manifest range the lockfile importer does not freeze, or a workspace member missing
from the lockfile is a finding.

Rule ids: `unpinned-rust-dependency`, `unpinned-node-dependency`,
`unpinned-python-dependency`.

Standard library only (`tomllib`), no network. The pnpm lockfile is YAML, which the
standard library cannot parse; `parse_pnpm_lock` implements the constrained subset
pnpm emits for the lockfile version and dependency sections the gate reads, and fails
closed (a finding) on anything it cannot parse.
"""
from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

from .finding import Finding
from .versions import is_wildcard, normalize_name, parse_requirement_name, satisfies

try:  # Python 3.11+
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the repository pins 3.12
    tomllib = None  # type: ignore[assignment]

RUST_RULE = "unpinned-rust-dependency"
NODE_RULE = "unpinned-node-dependency"
PYTHON_RULE = "unpinned-python-dependency"

CARGO_DEPENDENCY_SECTIONS = ("dependencies", "dev-dependencies", "build-dependencies")
NODE_DEPENDENCY_SECTIONS = (
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
)


def _toml(text: str, origin: str) -> Tuple[Optional[Dict[str, Any]], Optional[Finding]]:
    if tomllib is None:  # pragma: no cover
        return None, Finding(RUST_RULE, f"{origin}: tomllib unavailable; run with Python 3.11+")
    try:
        return tomllib.loads(text), None
    except tomllib.TOMLDecodeError as error:
        return None, Finding(RUST_RULE, f"{origin}: unparseable TOML: {error}")


def _lock_packages(data: Dict[str, Any]) -> Dict[str, List[Tuple[str, Optional[str]]]]:
    packages: Dict[str, List[Tuple[str, Optional[str]]]] = {}
    for entry in data.get("package", []):
        name = str(entry.get("name", "")).strip()
        if not name:
            continue
        packages.setdefault(name, []).append(
            (str(entry.get("version", "")).strip(), entry.get("source"))
        )
    return packages


def cargo_manifest_paths(root: Path) -> List[Path]:
    """Every workspace-member Cargo.toml (the root manifest when it is also a package)."""
    root_manifest = root / "Cargo.toml"
    if not root_manifest.exists():
        return []
    data, _ = _toml(root_manifest.read_text(), "Cargo.toml")
    if data is None:
        return [root_manifest]
    members = (data.get("workspace") or {}).get("members") or []
    paths: List[Path] = []
    if data.get("package") or not members:
        paths.append(root_manifest)
    for member in members:
        matches = sorted(path for path in root.glob(str(member)) if (path / "Cargo.toml").exists())
        paths.extend(match / "Cargo.toml" for match in matches)
    return paths


def _cargo_declared_dependencies(data: Dict[str, Any]) -> List[Tuple[str, Optional[str]]]:
    """(name, version-requirement or None) pairs from every manifest dependency section."""
    sections: List[Dict[str, Any]] = []
    for section in CARGO_DEPENDENCY_SECTIONS:
        sections.append(data.get(section) or {})
    workspace = data.get("workspace") or {}
    for section in CARGO_DEPENDENCY_SECTIONS:
        sections.append(workspace.get(section) or {})
    for target in (data.get("target") or {}).values():
        for section in CARGO_DEPENDENCY_SECTIONS:
            sections.append((target or {}).get(section) or {})
    declared: List[Tuple[str, Optional[str]]] = []
    for section in sections:
        for name, spec in section.items():
            if isinstance(spec, str):
                declared.append((name, spec))
            elif isinstance(spec, dict):
                declared.append((name, spec.get("version")))
            else:
                declared.append((name, None))
    return declared


def check_rust(root: Path) -> List[Finding]:
    """Rule: every Cargo dependency is pinned by Cargo.lock."""
    findings: List[Finding] = []
    lock_path = root / "Cargo.lock"
    manifests = cargo_manifest_paths(root)
    if not lock_path.exists():
        return [Finding(RUST_RULE, "Cargo.lock missing; Rust dependencies are not pinned")]
    data, error = _toml(lock_path.read_text(), "Cargo.lock")
    if error is not None or data is None:
        return [Finding(RUST_RULE, error.detail if error else "Cargo.lock unparseable")]

    packages = _lock_packages(data)
    for manifest in manifests:
        manifest_data, manifest_error = _toml(manifest.read_text(), str(manifest))
        rel = str(manifest.relative_to(root))
        if manifest_error is not None or manifest_data is None:
            findings.append(Finding(RUST_RULE, manifest_error.detail if manifest_error else rel))
            continue
        package_name = str((manifest_data.get("package") or {}).get("name", "")).strip()
        if package_name and package_name not in packages:
            findings.append(
                Finding(
                    RUST_RULE,
                    f"Cargo.lock: workspace member '{package_name}' ({rel}) is absent from the lockfile",
                )
            )
        for name, requirement in _cargo_declared_dependencies(manifest_data):
            if name not in packages:
                findings.append(
                    Finding(
                        RUST_RULE,
                        f"{rel}: declares '{name}' which Cargo.lock does not contain "
                        "(unlocked dependency; run `cargo generate-lockfile`)",
                    )
                )
                continue
            versions = [version for version, _ in packages[name]]
            if requirement is None or not str(requirement).strip():
                continue  # path-only dependency: the lockfile entry above is the pin
            if is_wildcard(requirement):
                findings.append(
                    Finding(RUST_RULE, f"{rel}: '{name}' uses floating requirement '{requirement}'")
                )
                continue
            if not any(satisfies(version, requirement) for version in versions):
                findings.append(
                    Finding(
                        RUST_RULE,
                        f"{rel}: '{name}' requirement '{requirement}' is not satisfied by "
                        f"Cargo.lock version(s) {versions}",
                    )
                )
    return findings


def parse_pnpm_lock(text: str) -> Dict[str, Dict[str, Dict[str, Dict[str, str]]]]:
    """Parse the `importers:` block of a pnpm lockfile into
    `{importer: {section: {name: {"specifier": ..., "version": ...}}}}`.

    Raises `ValueError` on structure the parser does not recognise, so the caller
    fails closed instead of silently treating the lockfile as pinned.
    """
    importers: Dict[str, Dict[str, Dict[str, Dict[str, str]]]] = {}
    lines = text.splitlines()
    start = next((i for i, line in enumerate(lines) if line.strip() == "importers:"), None)
    if start is None:
        raise ValueError("no top-level 'importers:' section")
    importer: Optional[str] = None
    section: Optional[str] = None
    name: Optional[str] = None
    for raw in lines[start + 1 :]:
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        indent = len(raw) - len(raw.lstrip(" "))
        if indent == 0:
            break  # next top-level key (`packages:`, `snapshots:`)
        stripped = raw.strip()
        if indent == 2:
            if stripped.endswith(": {}"):
                importer_path = stripped[: -len(": {}")].strip()
            elif stripped.endswith(":"):
                importer_path = stripped[:-1].strip()
            else:
                raise ValueError(f"unrecognised importer line: {stripped!r}")
            importer = importer_path.strip("'\"")
            importers.setdefault(importer, {})
            section, name = None, None
            continue
        if importer is None:
            raise ValueError(f"dependency line before any importer: {stripped!r}")
        if indent == 4:
            if not stripped.endswith(":"):
                raise ValueError(f"unrecognised importer section: {stripped!r}")
            section = stripped[:-1].strip()
            name = None
            continue
        if section is None:
            raise ValueError(f"dependency line before any section: {stripped!r}")
        if indent == 6:
            if not stripped.endswith(":"):
                raise ValueError(f"unrecognised dependency line: {stripped!r}")
            name = stripped[:-1].strip().strip("'\"")
            importers[importer].setdefault(section, {})[name] = {}
            continue
        if indent >= 8 and name is not None:
            key, _, value = stripped.partition(":")
            if key.strip() not in {"specifier", "version"}:
                continue
            importers[importer][section][name][key.strip()] = value.strip().strip("'\"")
            continue
        raise ValueError(f"unrecognised lockfile line: {stripped!r}")
    if not importers:
        raise ValueError("no importers found")
    return importers


def node_workspace_packages(root: Path) -> List[str]:
    """`package.json` directories from `pnpm-workspace.yaml` plus the root."""
    directories = ["."]
    workspace = root / "pnpm-workspace.yaml"
    if workspace.exists():
        in_packages = False
        for line in workspace.read_text().splitlines():
            if re.match(r"^packages:\s*$", line):
                in_packages = True
                continue
            if not in_packages:
                continue
            stripped = line.strip()
            if stripped.startswith("- "):
                pattern = stripped[2:].strip().strip("'\"")
                for match in sorted(root.glob(pattern)):
                    if (match / "package.json").exists():
                        directories.append(str(match.relative_to(root)))
            elif stripped and not line.startswith((" ", "\t")):
                break
    return sorted(set(directories))


def _node_manifest_dependencies(data: Dict[str, Any]) -> List[Tuple[str, str]]:
    declared: List[Tuple[str, str]] = []
    for section in NODE_DEPENDENCY_SECTIONS:
        table = data.get(section) or {}
        if not isinstance(table, dict):
            continue
        for name, range_text in table.items():
            declared.append((str(name), str(range_text)))
    return declared


def _locked_node_entry(locked: Dict[str, Dict[str, Dict[str, str]]], name: str):
    for section in NODE_DEPENDENCY_SECTIONS:
        entry = (locked.get(section) or {}).get(name)
        if entry is not None:
            return entry
    return None


def check_node(root: Path) -> List[Finding]:
    """Rule: every package.json dependency range is frozen in pnpm-lock.yaml."""
    findings: List[Finding] = []
    lock_path = root / "pnpm-lock.yaml"
    if not lock_path.exists():
        return [Finding(NODE_RULE, "pnpm-lock.yaml missing; TypeScript dependencies are not pinned")]
    try:
        importers = parse_pnpm_lock(lock_path.read_text())
    except ValueError as error:
        return [Finding(NODE_RULE, f"pnpm-lock.yaml: unparseable pnpm lockfile: {error}")]

    for directory in node_workspace_packages(root):
        manifest = root / directory / "package.json"
        if not manifest.exists():
            continue
        rel_manifest = str(manifest.relative_to(root))
        try:
            data = json.loads(manifest.read_text())
        except json.JSONDecodeError as error:
            findings.append(Finding(NODE_RULE, f"{rel_manifest}: unparseable package.json: {error}"))
            continue
        locked = importers.get(directory)
        if locked is None:
            findings.append(
                Finding(NODE_RULE, f"pnpm-lock.yaml: no importer for package '{directory}'")
            )
            continue
        for name, range_text in _node_manifest_dependencies(data):
            entry = _locked_node_entry(locked, name)
            if entry is None:
                findings.append(
                    Finding(
                        NODE_RULE,
                        f"{rel_manifest}: declares '{name}' which pnpm-lock.yaml does not contain "
                        "(unlocked dependency; run `pnpm install --lockfile-only`)",
                    )
                )
                continue
            specifier = entry.get("specifier")
            if specifier != range_text:
                findings.append(
                    Finding(
                        NODE_RULE,
                        f"{rel_manifest}: '{name}' range '{range_text}' is not frozen in the "
                        f"lockfile (lockfile specifier '{specifier}')",
                    )
                )
            version = entry.get("version", "")
            if not version:
                findings.append(
                    Finding(NODE_RULE, f"pnpm-lock.yaml: '{name}' has no resolved version")
                )
            else:
                # pnpm annotates peer resolution: `1.2.3(peer@4.5.6)`.
                base_version = version.split("(", 1)[0]
                if not is_wildcard(range_text) and not satisfies(base_version, range_text):
                    findings.append(
                        Finding(
                            NODE_RULE,
                            f"{rel_manifest}: '{name}' range '{range_text}' is not satisfied by "
                            f"locked version '{version}'",
                        )
                    )
    return findings


def _uv_first_party(data: Dict[str, Any], project_name: str) -> Optional[Dict[str, Any]]:
    target = normalize_name(project_name)
    for entry in data.get("package", []):
        if normalize_name(str(entry.get("name", ""))) == target:
            return entry
    return None


def _uv_specifier_map(requirements: Sequence[Any]) -> Dict[str, str]:
    """`{normalized name: specifier}` from uv.lock `requires-dist`/`requires-dev` entries."""
    locked: Dict[str, str] = {}
    for value in requirements or []:
        if not isinstance(value, dict):
            continue
        name = normalize_name(str(value.get("name", "")))
        if name:
            locked[name] = str(value.get("specifier", "")).strip()
    return locked


def check_python(root: Path) -> List[Finding]:
    """Rule: every pyproject dependency is pinned by python/uv.lock."""
    findings: List[Finding] = []
    package_dir = root / "python"
    manifest = package_dir / "pyproject.toml"
    lock_path = package_dir / "uv.lock"
    if not manifest.exists():
        return []
    if not lock_path.exists():
        return [Finding(PYTHON_RULE, "python/uv.lock missing; Python dependencies are not pinned")]
    manifest_data, manifest_error = _toml(manifest.read_text(), "python/pyproject.toml")
    if manifest_error is not None or manifest_data is None:
        return [Finding(PYTHON_RULE, manifest_error.detail if manifest_error else "bad manifest")]
    lock_data, lock_error = _toml(lock_path.read_text(), "python/uv.lock")
    if lock_error is not None or lock_data is None:
        return [Finding(PYTHON_RULE, lock_error.detail if lock_error else "bad lockfile")]

    packages = {
        normalize_name(str(entry.get("name", ""))): entry for entry in lock_data.get("package", [])
    }
    for entry in lock_data.get("package", []):
        name = str(entry.get("name", "")).strip() or "<unnamed>"
        if not str(entry.get("version", "")).strip():
            findings.append(Finding(PYTHON_RULE, f"python/uv.lock: '{name}' has no pinned version"))
        if not entry.get("source"):
            findings.append(Finding(PYTHON_RULE, f"python/uv.lock: '{name}' has no pinned source"))

    project = manifest_data.get("project") or {}
    project_name = str(project.get("name", "")).strip()
    first_party = _uv_first_party(lock_data, project_name) if project_name else None
    if first_party is None:
        findings.append(
            Finding(PYTHON_RULE, f"python/uv.lock: no package for project '{project_name}'")
        )
        return findings

    declared: List[Tuple[str, str, str, Optional[str]]] = []
    for requirement in project.get("dependencies") or []:
        name, specifier = parse_requirement_name(requirement)
        declared.append(("python/pyproject.toml", name, specifier, None))
    for group, requirements in (manifest_data.get("dependency-groups") or {}).items():
        for requirement in requirements or []:
            name, specifier = parse_requirement_name(requirement)
            declared.append((f"python/pyproject.toml [dependency-groups.{group}]", name, specifier, str(group)))

    metadata = first_party.get("metadata") or {}
    runtime_locked = _uv_specifier_map(metadata.get("requires-dist"))
    dev_locked: Dict[str, Dict[str, str]] = {
        str(group): _uv_specifier_map(requirements)
        for group, requirements in (metadata.get("requires-dev") or {}).items()
    }

    for origin, name, specifier, group in declared:
        if not specifier:
            findings.append(Finding(PYTHON_RULE, f"{origin}: '{name}' declares no version"))
            continue
        if is_wildcard(specifier):
            findings.append(
                Finding(PYTHON_RULE, f"{origin}: '{name}' uses floating requirement '{specifier}'")
            )
            continue
        locked_specifier = dev_locked.get(group or "", {}).get(name) if group else runtime_locked.get(name)
        if locked_specifier is None:
            findings.append(
                Finding(
                    PYTHON_RULE,
                    f"{origin}: declares '{name}' which python/uv.lock does not contain "
                    "(unlocked dependency; run `uv lock`)",
                )
            )
            continue
        if locked_specifier != specifier:
            findings.append(
                Finding(
                    PYTHON_RULE,
                    f"{origin}: '{name}' requirement '{specifier}' is not frozen in uv.lock "
                    f"(lockfile specifier '{locked_specifier}')",
                )
            )
            continue
        entry = packages.get(name)
        if entry is None or not satisfies(str(entry.get("version", "")), specifier):
            findings.append(
                Finding(
                    PYTHON_RULE,
                    f"{origin}: '{name}' requirement '{specifier}' is not satisfied by uv.lock "
                    f"version '{entry.get('version') if entry else None}'",
                )
            )
    return findings


def check(root: Path) -> List[Finding]:
    """All lockfile-pinning findings for `root`."""
    return check_rust(root) + check_node(root) + check_python(root)
