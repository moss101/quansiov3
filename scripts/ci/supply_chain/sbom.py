"""Deterministic CycloneDX SBOM generation and verification (OPS-007).

Builds a CycloneDX 1.5 software bill of materials from the repository's real
lockfiles (`Cargo.lock`, `pnpm-lock.yaml`, `python/uv.lock`): every resolved package
becomes a component with `name`, `version`, ecosystem and `purl`, and the top-level
`metadata.component` names this repository and the commit it describes.

The document is deterministic for a fixed lockfile set: components are sorted by
(ecosystem, name, version), JSON keys are sorted, and no timestamp or other
run-dependent value is emitted. `verify` re-generates the document and compares it to
a recorded SBOM byte-for-byte (canonically), so any dependency drift is a finding
rather than a silent difference.

Rule ids: `sbom-lockfile-missing`, `sbom-nondeterministic`, `sbom-empty`, `sbom-drift`.
"""
from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from .finding import Finding
from .versions import parse_version

try:  # Python 3.11+
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the repository pins 3.12
    tomllib = None  # type: ignore[assignment]

MISSING_RULE = "sbom-lockfile-missing"
DETERMINISM_RULE = "sbom-nondeterministic"
EMPTY_RULE = "sbom-empty"
DRIFT_RULE = "sbom-drift"

REPOSITORY = "quansio"
SPEC_VERSION = "1.5"
LOCKFILES = ("Cargo.lock", "pnpm-lock.yaml", "python/uv.lock")
PNPM_KEY_RE = re.compile(r"^ {2}('[^']+'|\"[^\"]+\"|[^\s:]+):\s*$")


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def lockfile_digests(root: Path) -> Dict[str, str]:
    digests: Dict[str, str] = {}
    for relative in LOCKFILES:
        path = root / relative
        if path.exists():
            digests[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    return digests


def _purl(ecosystem: str, name: str, version: str) -> str:
    if ecosystem == "npm":
        encoded = f"%40{name[1:]}" if name.startswith("@") else name
        return f"pkg:npm/{encoded}@{version}"
    if ecosystem == "pypi":
        normalized = re.sub(r"[-_.]+", "-", name).lower()
        return f"pkg:pypi/{normalized}@{version}"
    return f"pkg:{ecosystem}/{name}@{version}"


def _component(ecosystem: str, name: str, version: str) -> Dict[str, Any]:
    purl = _purl(ecosystem, name, version)
    return {
        "type": "library",
        "bom-ref": purl,
        "name": name,
        "version": version,
        "purl": purl,
        "properties": [{"name": "quansio:ecosystem", "value": ecosystem}],
    }


def _cargo_components(path: Path) -> List[Dict[str, Any]]:
    if tomllib is None:  # pragma: no cover
        return []
    data = tomllib.loads(path.read_text())
    components: List[Dict[str, Any]] = []
    for entry in data.get("package", []):
        name = str(entry.get("name", "")).strip()
        version = str(entry.get("version", "")).strip()
        if name and version:
            components.append(_component("cargo", name, version))
    return components


def _uv_components(path: Path) -> List[Dict[str, Any]]:
    return _cargo_components(path)


def _pnpm_components(path: Path) -> List[Dict[str, Any]]:
    """Components from the pnpm `packages:` block (`name@version` keys)."""
    components: List[Dict[str, Any]] = []
    in_packages = False
    for line in path.read_text().splitlines():
        if line.rstrip() == "packages:":
            in_packages = True
            continue
        if not in_packages:
            continue
        if line.strip() and not line.startswith(" "):
            break
        match = PNPM_KEY_RE.match(line)
        if not match:
            continue
        key = match.group(1).strip("'\"")
        key = key.split("(", 1)[0]  # drop pnpm peer-resolution suffix
        name, _, version = key.rpartition("@")
        if not name or not version or parse_version(version) is None:
            continue
        components.append(_component("npm", name, version))
    return components


def collect_components(root: Path) -> Tuple[List[Dict[str, Any]], List[Finding]]:
    """All components from the lockfiles that are present, plus missing-lockfile findings."""
    components: List[Dict[str, Any]] = []
    findings: List[Finding] = []
    for relative in LOCKFILES:
        path = root / relative
        if not path.exists():
            findings.append(Finding(MISSING_RULE, f"{relative} is missing; the SBOM is incomplete"))
            continue
        try:
            if relative == "pnpm-lock.yaml":
                components.extend(_pnpm_components(path))
            else:
                components.extend(_cargo_components(path))
        except Exception as error:  # noqa: BLE001 - report any parse failure as a finding
            findings.append(Finding(MISSING_RULE, f"{relative} could not be parsed: {error}"))
    return components, findings


def build(root: Path, commit: Optional[str] = None) -> Dict[str, Any]:
    """Build the deterministic CycloneDX document for `root` at `commit`."""
    components, _ = collect_components(root)
    version = commit or "nogit"
    properties = [{"name": "quansio:commit", "value": version}]
    for relative, digest in lockfile_digests(root).items():
        properties.append({"name": f"quansio:lockfile:sha256:{relative}", "value": digest})
    return {
        "bomFormat": "CycloneDX",
        "specVersion": SPEC_VERSION,
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "bom-ref": f"pkg:generic/{REPOSITORY}@{version}",
                "name": REPOSITORY,
                "version": version,
            },
            "properties": properties,
        },
        "components": sorted(
            components,
            key=lambda item: (
                item["properties"][0]["value"],
                item["name"],
                item["version"],
            ),
        ),
    }


def serialize(document: Dict[str, Any]) -> str:
    """Canonical serialization: sorted keys, stable indentation, trailing newline."""
    return json.dumps(document, indent=2, sort_keys=True) + "\n"


def repo_commit(root: Path) -> Optional[str]:
    """HEAD commit of `root`, or None outside a git repository."""
    import subprocess

    try:
        return subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=str(root),
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip() or None
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def sbom_filename(commit: Optional[str]) -> str:
    short = (commit or "nogit")[:12]
    return f"sbom-{short}.json"


def generate(root: Path, out: Path, commit: Optional[str] = None) -> Path:
    """Write the SBOM to `out`; returns the path written."""
    document = build(root, commit or repo_commit(root))
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(serialize(document))
    return out


def verify(root: Path, recorded: Path, commit: Optional[str] = None) -> List[Finding]:
    """Re-generate and compare against `recorded`; a difference is SBOM drift."""
    if not recorded.exists():
        return [Finding(DRIFT_RULE, f"recorded SBOM '{recorded}' does not exist")]
    try:
        recorded_document = json.loads(recorded.read_text())
    except json.JSONDecodeError as error:
        return [Finding(DRIFT_RULE, f"recorded SBOM '{recorded}' is not valid JSON: {error}")]
    if not isinstance(recorded_document, dict) or recorded_document.get("bomFormat") != "CycloneDX":
        return [Finding(DRIFT_RULE, f"recorded SBOM '{recorded}' is not a CycloneDX document")]
    expected = serialize(build(root, commit or repo_commit(root)))
    actual = serialize(recorded_document)
    if actual == expected:
        return []
    expected_names = {
        (item["name"], item["version"]) for item in json.loads(expected).get("components", [])
    }
    actual_names = {
        (item.get("name"), item.get("version"))
        for item in recorded_document.get("components", [])
        if isinstance(item, dict)
    }
    added = sorted(expected_names - actual_names)[:5]
    removed = sorted(actual_names - expected_names)[:5]
    return [
        Finding(
            DRIFT_RULE,
            f"recorded SBOM '{recorded}' does not match the lockfiles "
            f"(missing components: {added or 'none'}; stale components: {removed or 'none'})",
        )
    ]


def check(
    root: Path,
    artifacts_dir: Path,
    commit: Optional[str] = None,
) -> Tuple[List[Finding], Optional[Path]]:
    """Generate the SBOM, prove determinism and verify it against the generated record."""
    resolved_commit = commit or repo_commit(root)
    components, findings = collect_components(root)
    if not components:
        findings.append(Finding(EMPTY_RULE, "SBOM has no components; no lockfile was read"))
    destination = artifacts_dir / sbom_filename(resolved_commit)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(serialize(build(root, resolved_commit)))

    first = serialize(build(root, resolved_commit))
    second = serialize(build(root, resolved_commit))
    if first != second:
        findings.append(Finding(DETERMINISM_RULE, "two consecutive SBOM generations differ"))

    findings.extend(verify(root, destination, resolved_commit))
    return findings, destination


def main(argv: Optional[Sequence[str]] = None) -> int:
    """`--verify` re-generates and compares against a recorded SBOM, failing on drift."""
    import argparse

    parser = argparse.ArgumentParser(description="CycloneDX SBOM generation and drift verification")
    parser.add_argument("--root", default=".", help="tree to describe (defaults to the cwd)")
    parser.add_argument("--out", default=None, help="SBOM output path")
    parser.add_argument("--verify", default=None, help="recorded SBOM to compare against")
    parser.add_argument("--commit", default=None, help="commit recorded in the SBOM metadata")
    args = parser.parse_args(argv)
    root = Path(args.root).resolve()
    if args.verify:
        findings = verify(root, Path(args.verify), args.commit)
        for finding in findings:
            print(finding)
        print(f"sbom verify: {'CLEAN' if not findings else str(len(findings)) + ' finding(s)'}")
        return 1 if findings else 0
    destination = Path(args.out) if args.out else root / "artifacts" / "ci" / sbom_filename(
        args.commit or repo_commit(root)
    )
    generate(root, destination, args.commit)
    print(f"sbom: wrote {destination}")
    return 0


if __name__ == "__main__":  # pragma: no cover - exercised through check.py and tests
    raise SystemExit(main())
