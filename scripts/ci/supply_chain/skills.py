"""Untrusted skill/tool quarantine lifecycle scan (OPS-007).

Imported skill, pack and tool artifacts must pass the controlled promotion path
before they may influence production context. DOSSIER.md section 16 requires
`quarantine on import; preview sandboxing; eval gates`; DOSSIER.md section 13 fixes
the promotion sequence `verified evidence -> candidate patch -> evaluation ->
governed promotion`; DOMAIN.md section 11.5 defines `SkillVersion` provenance/status
and the states `DRAFT -> CANDIDATE -> EVALUATING -> APPROVED -> ACTIVE -> DEPRECATED
-> RETIRED`. This gate makes that lifecycle checkable on disk.

A manifest under `packs/` (or any skill/tool manifest anywhere in the tree) must carry
all six lifecycle sections:

  quarantine    {status, imported_at}
  provenance    {source, ref, digest}
  review        {reviewed_by, reviewed_at, outcome}
  normalization {normalized_at, normalizer}
  evaluation    {eval_suite_id, result, evaluated_at}
  approved      {approved_by, approved_at}

When a manifest's own vocabulary is closed elsewhere with no room for a governance
record (`skill.yaml` / `SkillManifest`), the six sections and `status` are read from
a sibling `meta.yaml` identity/governance file instead -- see `GOVERNANCE_SIDECAR`.
A manifest with no such sibling (e.g. `pack.yaml`) carries them inline.

A manifest that is `active`/`approved` while provenance, review, normalization,
evaluation or approval is empty, or whose quarantine has not been cleared, is a
self-promotion and is refused (skills cannot silently promote their own updates).

Rule ids: `skill-lifecycle-incomplete`, `skill-self-promoted`,
`skill-manifest-unparsable`. Test fixtures under `tests/ci/fixtures/` are excluded
from the live tree scan; they are exercised directly by `tests/ci/test_supply_chain.py`
so a non-quarantined manifest is proven to be flagged without shipping one as a real
pack.
"""
from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

from .finding import Finding

try:  # Python 3.11+
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the repository pins 3.12
    tomllib = None  # type: ignore[assignment]

LIFECYCLE_RULE = "skill-lifecycle-incomplete"
SELF_PROMOTION_RULE = "skill-self-promoted"
UNPARSABLE_RULE = "skill-manifest-unparsable"

SUPPORTED_SUFFIXES = (".json", ".toml", ".yaml", ".yml")
# A filename token (`-`/`_`/`.`-separated) that marks a skill/tool manifest anywhere.
SKILL_TOOL_TOKENS = frozenset({"skill", "tool"})
# Pack-level manifest tokens; only recognised under `packs/` so the repository's root
# MANIFEST.json is not mistaken for a pack manifest.
PACK_ONLY_TOKENS = frozenset({"manifest", "pack", "capability"})

LIFECYCLE_SECTIONS: Dict[str, Tuple[str, ...]] = {
    "quarantine": ("status", "imported_at"),
    "provenance": ("source", "ref", "digest"),
    "review": ("reviewed_by", "reviewed_at", "outcome"),
    "normalization": ("normalized_at", "normalizer"),
    "evaluation": ("eval_suite_id", "result", "evaluated_at"),
    "approved": ("approved_by", "approved_at"),
}
ACTIVE_STATES = frozenset({"active", "approved", "published"})
CLEARED_QUARANTINE = frozenset({"cleared", "approved", "verified", "released", "complete"})
#: In a governance sidecar (`meta.yaml`), the six sections nest under this one key
#: instead of the top level, so they cannot collide with the sidecar's own existing
#: fields -- `meta.yaml` already has a scalar `provenance: pack://...` identity
#: pointer, a different shape than the `provenance: {source, ref, digest}` section
#: below. `status` is still read from the sidecar's top level (already compatible:
#: `meta.yaml` has carried a top-level `status` since INT-009).
GOVERNANCE_RECORD_KEY = "quarantine_record"

EXCLUDED_DIR_SEGMENTS = frozenset(
    {
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        "coverage",
        "__pycache__",
        ".venv",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        ".pnpm-store",
        ".turbo",
        ".next",
    }
)
# Excluded only as the repository's OWN top-level directory (`<root>/artifacts/`,
# generated CI summaries/SBOMs — never a manifest). Unlike the vendor/build-output
# names above, "artifacts" is also a real product noun (DOMAIN.md's Artifact
# entity), so a package/skill legitimately named `artifacts` — packs/capabilities/
# artifacts/, packs/skills/artifacts/ — must still be scanned. Matching this name
# at any path depth silently exempted exactly those two manifests from the
# quarantine lifecycle; a `skill-lifecycle-incomplete`/`skill-self-promoted`
# false negative on a real package is a fail-open, not a cosmetic miss.
TOP_LEVEL_EXCLUDED_DIRS = frozenset({"artifacts"})
FIXTURE_PREFIX = "tests/ci/fixtures/"


def _is_empty(value: Any) -> bool:
    if value is None:
        return True
    if isinstance(value, str):
        return not value.strip()
    if isinstance(value, (list, tuple, set, dict)):
        return len(value) == 0 or all(_is_empty(item) for item in value)
    return False


def _parse(path: Path) -> Tuple[Optional[Dict[str, Any]], Optional[str]]:
    suffix = path.suffix.lower()
    try:
        text = path.read_text()
    except OSError as error:
        return None, f"cannot read manifest: {error}"
    if suffix == ".json":
        try:
            data = json.loads(text)
        except json.JSONDecodeError as error:
            return None, f"invalid JSON: {error}"
    elif suffix == ".toml":
        if tomllib is None:  # pragma: no cover
            return None, "tomllib unavailable; run with Python 3.11+"
        try:
            data = tomllib.loads(text)
        except tomllib.TOMLDecodeError as error:
            return None, f"invalid TOML: {error}"
    else:
        try:
            import yaml  # type: ignore[import-not-found]
        except ModuleNotFoundError:
            return None, (
                f"{suffix} manifests cannot be parsed without PyYAML; quarantine cannot be "
                "verified, so the gate fails closed"
            )
        try:
            data = yaml.safe_load(text)
        except Exception as error:  # noqa: BLE001 - any YAML failure fails closed
            return None, f"invalid YAML: {error}"
    if not isinstance(data, dict):
        return None, "manifest root is not a mapping"
    return data, None


def _stem_tokens(path: Path) -> frozenset:
    return frozenset(re.split(r"[-_.]+", path.stem.lower()))


def is_manifest(path: Path, root: Path) -> bool:
    """True when `path` is a skill/tool/pack manifest the lifecycle applies to."""
    if path.suffix.lower() not in SUPPORTED_SUFFIXES:
        return False
    tokens = _stem_tokens(path)
    if tokens & SKILL_TOOL_TOKENS:
        return True
    relative = path.relative_to(root).as_posix() if root in path.parents else path.name
    return relative.startswith("packs/") and bool(tokens & PACK_ONLY_TOKENS)


def _walk(root: Path) -> Iterable[Path]:
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if any(part in EXCLUDED_DIR_SEGMENTS for part in path.parts):
            continue
        relative_parts = path.relative_to(root).parts if root in path.parents else ()
        if relative_parts and relative_parts[0] in TOP_LEVEL_EXCLUDED_DIRS:
            continue
        yield path


def manifest_paths(root: Path) -> List[str]:
    """Repository-relative skill/tool manifest paths in `root`."""
    found: List[str] = []
    for path in _walk(root):
        relative = path.relative_to(root).as_posix()
        if relative.startswith(FIXTURE_PREFIX):
            continue  # negative fixtures are exercised directly by the test suite
        if is_manifest(path, root):
            found.append(relative)
    return found


def _status(manifest: Dict[str, Any]) -> str:
    for key in ("status", "state", "lifecycle_status"):
        value = manifest.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip().lower()
    return ""


def _lifecycle_findings(
    relative: str, manifest: Dict[str, Any], sections: Optional[Dict[str, Any]] = None
) -> List[Finding]:
    """Lifecycle findings for `manifest`'s status against `sections` (its own top
    level when `sections` is not given -- the inline case; a sidecar's nested
    governance record otherwise, see `GOVERNANCE_RECORD_KEY`)."""
    sections = manifest if sections is None else sections
    findings: List[Finding] = []
    missing: List[str] = []
    for section, fields in LIFECYCLE_SECTIONS.items():
        value = sections.get(section)
        if not isinstance(value, dict):
            missing.append(section)
            continue
        for field in fields:
            if _is_empty(value.get(field)):
                missing.append(f"{section}.{field}")
    if missing:
        findings.append(
            Finding(
                LIFECYCLE_RULE,
                f"{relative}: missing quarantine lifecycle fields {sorted(set(missing))} "
                "(DOSSIER.md section 16 / DOMAIN.md section 11.5)",
            )
        )

    status = _status(manifest)
    quarantine = sections.get("quarantine") if isinstance(sections.get("quarantine"), dict) else {}
    quarantine_status = str((quarantine or {}).get("status", "")).strip().lower()
    incomplete_sections = [
        section for section in ("provenance", "review", "normalization", "evaluation", "approved")
        if _is_empty(sections.get(section))
    ]
    if status in ACTIVE_STATES and (incomplete_sections or quarantine_status not in CLEARED_QUARANTINE):
        detail = "empty " + ", ".join(incomplete_sections) if incomplete_sections else ""
        if quarantine_status not in CLEARED_QUARANTINE:
            detail = f"quarantine status '{quarantine_status or 'missing'}' not cleared"
        findings.append(
            Finding(
                SELF_PROMOTION_RULE,
                f"{relative}: status '{status}' with {detail}; skills cannot silently promote "
                "their own updates (quarantine -> provenance -> review -> normalization -> "
                "evaluation -> approved)",
            )
        )
    return findings


#: A behaviour manifest's own vocabulary can be closed and enforced elsewhere (for
#: example `SkillManifest` in `python/intelligence/skills/models.py`, which refuses
#: any key outside its documented seven) with no room for a governance record. When
#: a sibling `meta.yaml` exists next to a detected manifest, it is the identity/
#: governance sidecar -- lifecycle and status are read from *it* instead of the
#: behaviour file, exactly like the built-in packs already separate `skill.yaml`
#: (what a skill does, closed vocabulary) from `meta.yaml` (owner, semver, status,
#: provenance). A manifest with no such sibling (e.g. `pack.yaml`, or any imported
#: manifest with no separate identity file) still carries the six sections inline.
GOVERNANCE_SIDECAR = "meta.yaml"


def scan(root: Path) -> List[Finding]:
    """Lifecycle findings for every skill/tool manifest under `root`."""
    findings: List[Finding] = []
    for relative in manifest_paths(root):
        manifest_path = root / relative
        manifest, error = _parse(manifest_path)
        if error is not None or manifest is None:
            findings.append(Finding(UNPARSABLE_RULE, f"{relative}: {error}"))
            continue

        sidecar_path = manifest_path.parent / GOVERNANCE_SIDECAR
        if manifest_path.name != GOVERNANCE_SIDECAR and sidecar_path.is_file():
            sidecar_relative = sidecar_path.relative_to(root).as_posix()
            governance, sidecar_error = _parse(sidecar_path)
            if sidecar_error is not None or governance is None:
                findings.append(Finding(UNPARSABLE_RULE, f"{sidecar_relative}: {sidecar_error}"))
                continue
            record = governance.get(GOVERNANCE_RECORD_KEY)
            sections = record if isinstance(record, dict) else {}
            findings.extend(_lifecycle_findings(sidecar_relative, governance, sections))
            continue

        findings.extend(_lifecycle_findings(relative, manifest))
    return findings
