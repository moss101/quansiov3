"""Known-vulnerable dependency scan (OPS-007).

Scans resolved lockfile packages against a committed, offline advisory corpus in
RustSec advisory-file shape (`[advisory]` + `[versions]` TOML, the format the RustSec
advisory-db ships converted from its Markdown front matter). No network access is
required or attempted; the corpus under
`tests/ci/fixtures/supply_chain/advisory-db/` is the fixture that drives the rule.

A package is flagged when its exact resolved version matches an advisory for that
package and is neither in the advisory's `patched` nor its `unaffected` range. The
match is version-exact off the lockfile, so the result is reproducible.

Fixture provenance (do not replace with an invented identifier): the committed
record `RUSTSEC-2019-0014` (`CVE-2019-16138`, `GHSA-m2pf-hprp-3vqm`, CVSS 9.8
CRITICAL, patched `>=0.21.3`, unaffected `<0.10.2`) is a real published advisory for
the real crate `image`. The paired fixture `tests/ci/fixtures/supply_chain/
vulnerable-cargo.lock` pins `image 0.21.2`, which lies in the advisory's affected
range `<0.21.3, >=0.10.2`, so it is a genuine match rather than a synthetic one.

Rule id: `vulnerable-dependency`.
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

from .finding import Finding
from .versions import normalize_name, satisfies

try:  # Python 3.11+
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the repository pins 3.12
    tomllib = None  # type: ignore[assignment]

RULE = "vulnerable-dependency"


@dataclass(frozen=True)
class Advisory:
    id: str
    package: str
    title: str = ""
    url: str = ""
    aliases: Tuple[str, ...] = ()
    patched: Tuple[str, ...] = ()
    unaffected: Tuple[str, ...] = ()
    severity: str = ""
    cvss: str = ""

    def matches(self, version: str) -> bool:
        """True when `version` of this advisory's package is affected."""
        if any(satisfies(version, requirement) for requirement in self.unaffected):
            return False
        if self.patched:
            return not any(satisfies(version, requirement) for requirement in self.patched)
        # No patched release: the whole declared range is affected.
        return True

    def describe(self) -> str:
        bits = [self.id]
        if self.aliases:
            bits.append("/".join(self.aliases))
        if self.severity:
            bits.append(self.severity)
        elif self.cvss:
            bits.append(self.cvss)
        if self.url:
            bits.append(self.url)
        title = f" {self.title}" if self.title else ""
        return f"{' '.join(bits)}:{title}"


def load_advisories(directory: Path) -> Tuple[List[Advisory], List[Finding]]:
    """Parse every `*.toml` advisory under `directory` (recursive, sorted)."""
    advisories: List[Advisory] = []
    findings: List[Finding] = []
    if not directory.exists():
        return advisories, [
            Finding(RULE, f"advisory database '{directory}' is missing; cannot prove the scan ran")
        ]
    for path in sorted(directory.rglob("*.toml")):
        if tomllib is None:  # pragma: no cover
            findings.append(Finding(RULE, f"{path}: tomllib unavailable"))
            continue
        try:
            data: Dict[str, Any] = tomllib.loads(path.read_text())
        except tomllib.TOMLDecodeError as error:
            findings.append(Finding(RULE, f"{path}: unparseable advisory record: {error}"))
            continue
        advisory = data.get("advisory") or {}
        versions = data.get("versions") or {}
        identifier = str(advisory.get("id", "")).strip()
        package = str(advisory.get("package", "")).strip()
        if not identifier or not package:
            findings.append(
                Finding(RULE, f"{path}: advisory record needs `id` and `package`")
            )
            continue
        advisories.append(
            Advisory(
                id=identifier,
                package=package,
                title=str(advisory.get("title", "")).strip(),
                url=str(advisory.get("url", "")).strip(),
                aliases=tuple(str(alias) for alias in advisory.get("aliases") or []),
                patched=tuple(str(item) for item in versions.get("patched") or []),
                unaffected=tuple(str(item) for item in versions.get("unaffected") or []),
                severity=str(advisory.get("severity", "")).strip().lower(),
                cvss=str(advisory.get("cvss", "")).strip(),
            )
        )
    return advisories, findings


def _locked_packages(lockfile: Path) -> List[Tuple[str, str]]:
    if tomllib is None:  # pragma: no cover
        return []
    data = tomllib.loads(lockfile.read_text())
    packages: List[Tuple[str, str]] = []
    for entry in data.get("package", []):
        name = str(entry.get("name", "")).strip()
        version = str(entry.get("version", "")).strip()
        if name and version:
            packages.append((name, version))
    return packages


def scan_lockfile(lockfile: Path, advisories: Sequence[Advisory]) -> List[Finding]:
    """Flag every resolved package in `lockfile` that matches a known advisory."""
    findings: List[Finding] = []
    if not lockfile.exists():
        return [Finding(RULE, f"{lockfile} is missing; the vulnerability scan cannot run")]
    try:
        packages = _locked_packages(lockfile)
    except Exception as error:  # noqa: BLE001 - report any parse failure as a finding
        return [Finding(RULE, f"{lockfile}: unparseable lockfile: {error}")]
    by_name: Dict[str, List[Advisory]] = {}
    for advisory in advisories:
        by_name.setdefault(normalize_name(advisory.package), []).append(advisory)
    for name, version in packages:
        for advisory in by_name.get(normalize_name(name), []):
            if advisory.matches(version):
                findings.append(
                    Finding(
                        RULE,
                        f"{lockfile}: {name} {version} matches {advisory.describe()}",
                    )
                )
    return findings


def lockfiles_in(root: Path) -> List[Path]:
    """The lockfiles the OSV/RustSec scan covers."""
    return [path for path in (root / "Cargo.lock", root / "python" / "uv.lock") if path.exists()]


def scan(
    root: Path,
    advisory_dir: Path,
    cargo_lock: Optional[Path] = None,
) -> List[Finding]:
    """Scan the repository's resolved dependency sets against the advisory corpus."""
    advisories, findings = load_advisories(advisory_dir)
    if cargo_lock is not None:
        return findings + scan_lockfile(cargo_lock, advisories)
    for lockfile in lockfiles_in(root):
        findings.extend(scan_lockfile(lockfile, advisories))
    return findings
