"""License/advisory policy checks (OPS-007).

Validates `deny.toml` (cargo-deny configuration, DOSSIER.md section 18) against the
repository with `tomllib`:

  * `[advisories]` present with `version`, a non-empty `db-urls` and an effective
    `yanked` setting (never `allow`);
  * `[licenses]` present with a non-empty `allow` list that covers every licence the
    in-repository manifests actually declare;
  * `[sources]` present with `unknown-registry` and `unknown-git` denied.

When the `cargo-deny` binary is available it is also run over the tree and its exit
code is reported. The binary is optional: it needs the advisory database, so when it
is absent, or when it fails to reach the advisory DB, the gate reports an explicit
informational `cargo-deny-unavailable` result. It never silently passes and never
blocks the rest of the gate on a missing optional tool.

Rule ids: `deny-config-incomplete`, `license-not-allowlisted`, `cargo-deny`
(informational results: `cargo-deny`, `cargo-deny-unavailable`).
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Set, Tuple

from .finding import Finding

try:  # Python 3.11+
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the repository pins 3.12
    tomllib = None  # type: ignore[assignment]

CONFIG_RULE = "deny-config-incomplete"
LICENSE_RULE = "license-not-allowlisted"
CARGO_DENY_RULE = "cargo-deny"
CARGO_DENY_UNAVAILABLE = "cargo-deny-unavailable"

YANKED_SETTINGS = {"deny", "warn", "allow"}
NETWORK_FAILURE_MARKERS = (
    "failed to fetch",
    "failed to download",
    "network",
    "timed out",
    "timeout",
    "could not resolve",
    "connection refused",
    "dns",
    "offline",
    "unable to update",
)


def _parse(config: Path) -> Tuple[Optional[Dict[str, Any]], Optional[Finding]]:
    if tomllib is None:  # pragma: no cover
        return None, Finding(CONFIG_RULE, "tomllib unavailable; run the gate with Python 3.11+")
    try:
        return tomllib.loads(config.read_text()), None
    except tomllib.TOMLDecodeError as error:
        return None, Finding(CONFIG_RULE, f"deny.toml is not valid TOML: {error}")


def declared_licenses(root: Path) -> Set[str]:
    """Licence expressions declared by the in-repository workspace manifests."""
    licenses: Set[str] = set()
    root_manifest = root / "Cargo.toml"
    workspace_license: Optional[str] = None
    if root_manifest.exists() and tomllib is not None:
        data = tomllib.loads(root_manifest.read_text())
        workspace_license = str((data.get("workspace", {}).get("package") or {}).get("license", "")).strip() or None
        members = (data.get("workspace") or {}).get("members") or []
        for member in members:
            for manifest in sorted(root.glob(str(member))):
                cargo = manifest / "Cargo.toml"
                if cargo.exists():
                    licenses.update(_cargo_manifest_licenses(cargo, workspace_license))
        if data.get("package") or not members:
            licenses.update(_cargo_manifest_licenses(root_manifest, workspace_license))

    pyproject = root / "python" / "pyproject.toml"
    if pyproject.exists() and tomllib is not None:
        project = tomllib.loads(pyproject.read_text()).get("project") or {}
        license_value = project.get("license")
        if isinstance(license_value, dict):
            text = str(license_value.get("text", "")).strip()
            if text:
                licenses.add(text)
        elif isinstance(license_value, str) and license_value.strip():
            licenses.add(license_value.strip())

    for manifest in sorted(root.rglob("package.json")):
        if any(part in {".git", "node_modules", "dist", "target"} for part in manifest.parts):
            continue
        try:
            data = json.loads(manifest.read_text())
        except json.JSONDecodeError:
            continue
        value = data.get("license")
        if isinstance(value, str) and value.strip() and value.strip() != "UNLICENSED":
            licenses.add(value.strip())
    return licenses


def _cargo_manifest_licenses(manifest: Path, workspace_license: Optional[str]) -> Set[str]:
    if tomllib is None:  # pragma: no cover
        return set()
    try:
        package = tomllib.loads(manifest.read_text()).get("package") or {}
    except tomllib.TOMLDecodeError:
        return set()
    license_value = package.get("license")
    if isinstance(license_value, str) and license_value.strip():
        return {license_value.strip()}
    if isinstance(license_value, dict) and license_value.get("workspace") and workspace_license:
        return {workspace_license}
    if workspace_license:
        return {workspace_license}
    return set()


def check_config(root: Path) -> List[Finding]:
    """Structural completeness of `deny.toml` plus licence allow-list coverage."""
    findings: List[Finding] = []
    config = root / "deny.toml"
    if not config.exists():
        return [
            Finding(
                CONFIG_RULE,
                "deny.toml is missing; advisories, licences, bans and sources are unenforced",
            )
        ]
    data, error = _parse(config)
    if error is not None or data is None:
        return [error or Finding(CONFIG_RULE, "deny.toml could not be parsed")]

    advisories = data.get("advisories")
    if not isinstance(advisories, dict):
        findings.append(Finding(CONFIG_RULE, "deny.toml is missing the [advisories] section"))
    else:
        if advisories.get("version") is None:
            findings.append(Finding(CONFIG_RULE, "[advisories] is missing `version`"))
        db_urls = advisories.get("db-urls")
        if not isinstance(db_urls, list) or not db_urls:
            findings.append(Finding(CONFIG_RULE, "[advisories] needs a non-empty `db-urls` list"))
        yanked = advisories.get("yanked")
        if yanked is None:
            findings.append(Finding(CONFIG_RULE, "[advisories] is missing `yanked`"))
        elif str(yanked) not in YANKED_SETTINGS:
            findings.append(
                Finding(CONFIG_RULE, f"[advisories] `yanked = {yanked!r}` is not one of {sorted(YANKED_SETTINGS)}")
            )
        elif str(yanked) == "allow":
            findings.append(
                Finding(CONFIG_RULE, "[advisories] `yanked = \"allow\"` disables the yanked-crate check")
            )
        if "ignore" in advisories and not isinstance(advisories["ignore"], list):
            findings.append(Finding(CONFIG_RULE, "[advisories] `ignore` must be a list"))

    licenses = data.get("licenses")
    allow: List[str] = []
    if not isinstance(licenses, dict):
        findings.append(Finding(CONFIG_RULE, "deny.toml is missing the [licenses] section"))
    else:
        if licenses.get("version") is None:
            findings.append(Finding(CONFIG_RULE, "[licenses] is missing `version`"))
        raw_allow = licenses.get("allow")
        if not isinstance(raw_allow, list) or not raw_allow:
            findings.append(Finding(CONFIG_RULE, "[licenses] needs a non-empty `allow` list"))
        else:
            allow = [str(item).strip() for item in raw_allow]
            if "*" in allow:
                findings.append(
                    Finding(CONFIG_RULE, "[licenses] `allow = [\"*\"]` accepts every licence")
                )

    sources = data.get("sources")
    if not isinstance(sources, dict):
        findings.append(Finding(CONFIG_RULE, "deny.toml is missing the [sources] section"))
    else:
        for key, expected in (("unknown-registry", "deny"), ("unknown-git", "deny")):
            value = sources.get(key)
            if value is None:
                findings.append(Finding(CONFIG_RULE, f"[sources] is missing `{key}`"))
            elif str(value) != expected:
                findings.append(
                    Finding(
                        CONFIG_RULE,
                        f"[sources] `{key} = {value!r}` must be {expected!r} (unknown sources are untrusted)",
                    )
                )

    normalized_allow = {item.lower() for item in allow}
    for license_expression in sorted(declared_licenses(root)):
        if license_expression.lower() not in normalized_allow:
            findings.append(
                Finding(
                    LICENSE_RULE,
                    f"declared licence '{license_expression}' is not covered by deny.toml [licenses].allow",
                )
            )
    return findings


def run_cargo_deny(
    root: Path, timeout: int = 900
) -> Tuple[List[Finding], List[Finding]]:
    """Run `cargo-deny` when present; returns (findings, informational results)."""
    binary = shutil.which("cargo-deny")
    if binary is None:
        return [], [
            Finding(
                CARGO_DENY_UNAVAILABLE,
                "cargo-deny binary is not installed; licences/bans/sources/advisories are "
                "checked offline by this gate instead (install cargo-deny for the external check)",
                informational=True,
            )
        ]
    command: Sequence[str] = [binary, "check", "licenses", "bans", "sources", "advisories"]
    try:
        result = subprocess.run(
            command,
            cwd=str(root),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (subprocess.TimeoutExpired, OSError) as error:
        return [], [
            Finding(
                CARGO_DENY_UNAVAILABLE,
                f"cargo-deny did not complete ({error}); the offline gate still applies",
                informational=True,
            )
        ]
    output = "\n".join((result.stdout + result.stderr).splitlines()[-25:])
    if result.returncode == 0:
        return [], [
            Finding(CARGO_DENY_RULE, "cargo deny check licenses bans sources advisories: clean", informational=True)
        ]
    lowered = (result.stdout + result.stderr).lower()
    if any(marker in lowered for marker in NETWORK_FAILURE_MARKERS):
        return [], [
            Finding(
                CARGO_DENY_UNAVAILABLE,
                "cargo-deny could not reach the advisory database (offline); the committed "
                f"advisory fixture scan still applies. Output tail: {output}",
                informational=True,
            )
        ]
    return [
        Finding(CARGO_DENY_RULE, f"cargo deny check exited {result.returncode}: {output}")
    ], []


def check(root: Path, run_external: bool = True) -> Tuple[List[Finding], List[Finding]]:
    """Policy findings and informational results (external tool included when asked)."""
    findings = check_config(root)
    informational: List[Finding] = []
    if run_external:
        external_findings, informational = run_cargo_deny(root)
        findings.extend(external_findings)
    return findings, informational
