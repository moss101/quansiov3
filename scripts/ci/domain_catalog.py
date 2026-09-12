#!/usr/bin/env python3
"""Parse the canonical domain model (DOMAIN.md) into generated contract catalogs.

DOMAIN.md is the naming and shape authority; generated contracts MUST derive from it
(DOMAIN.md preamble, §17). This module is the single derivation step:

    DOMAIN.md  ->  schemas/catalog/*.yaml  ->  OpenAPI / JSON Schema / bindings

Nothing here invents names: every value comes from a table in DOMAIN.md. If a name
disappears from DOMAIN.md the regeneration diff fails, which is the drift gate.

Python 3.11+ ; standard library only (PyYAML for writing catalogs).
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Dict, List, Optional

ROOT = Path(__file__).resolve().parents[2]
DOMAIN = ROOT / "DOMAIN.md"
CATALOG_DIR = ROOT / "schemas" / "catalog"


def _section(text: str, heading: str) -> str:
    """Body of `heading` up to the next `## ` heading."""
    start = text.index(heading)
    rest = text[start + len(heading) :]
    end = rest.find("\n## ")
    return rest if end == -1 else rest[:end]


def _table_rows(body: str) -> List[List[str]]:
    rows: List[List[str]] = []
    for line in body.splitlines():
        stripped = line.strip()
        if not stripped.startswith("|"):
            continue
        cells = [c.strip() for c in stripped.strip("|").split("|")]
        if not cells or set("".join(cells)) <= set("-: "):
            continue
        rows.append(cells)
    return rows


def parse_glossary(text: str) -> List[str]:
    """§0 glossary terms (Quansio-native terminology)."""
    terms: List[str] = []
    for cells in _table_rows(_section(text, "## 0. Glossary")):
        match = re.match(r"\*\*(.+?)\*\*", cells[0])
        if match:
            terms.append(match.group(1))
    return terms


def parse_id_prefixes(text: str) -> Dict[str, str]:
    """§1.1 entity -> canonical id prefix. The table is printed in two column pairs."""
    prefixes: Dict[str, str] = {}
    for cells in _table_rows(_section(text, "### 1.1 Canonical IDs")):
        i = 0
        while i < len(cells) - 1:
            name_cell = cells[i].strip()
            prefix_cell = cells[i + 1].strip()
            matched = False
            for entity, prefix in _name_prefix_pairs(name_cell, prefix_cell):
                if entity and re.fullmatch(r"[a-z]{2,4}_", prefix):
                    prefixes[entity] = prefix
                    matched = True
            i += 2 if matched else 1
    return prefixes


def _name_prefix_pairs(name_cell: str, prefix_cell: str) -> List[tuple]:
    """Split compound rows such as `Skill / SkillVersion` with `skl_ / sklv_`."""
    names = [re.sub(r"\s*\(.*\)$", "", part).strip() for part in name_cell.split("/")]
    prefix_parts = [part.strip().strip("`") for part in prefix_cell.split("/")]
    if len(names) == len(prefix_parts) and len(names) > 1:
        expanded = [names[0]]
        for extra in names[1:]:
            expanded.append(extra if extra.startswith(names[0]) else names[0] + extra)
        return list(zip(expanded, prefix_parts))
    return [(re.sub(r"\s*\(.*\)$", "", name_cell).strip(), prefix_cell.strip().strip("`"))]


def parse_commands(text: str) -> Dict[str, List[str]]:
    """§14 public command catalog: family -> command names."""
    families: Dict[str, List[str]] = {}
    for cells in _table_rows(_section(text, "## 14. Public command catalog")):
        family = cells[0].strip()
        if not family or family.lower() in {"family", "read projections"}:
            continue
        names = re.findall(r"`([A-Z][A-Za-z0-9]+)`", cells[1]) if len(cells) > 1 else []
        families[family] = names
    return families


def parse_read_projections(text: str) -> List[str]:
    """§14 read projection paths, read from the `Read projections` row only."""
    body = _section(text, "## 14. Public command catalog")
    row = None
    for cells in _table_rows(body):
        if cells[0].strip().lower() == "read projections":
            row = cells[1]
            break
    if row is None:
        return []
    paths: List[str] = []
    for token in re.findall(r"`([^`]+)`", row):
        token = token.strip()
        if token.startswith("GET "):
            token = token[4:].strip()
        if not token.startswith("/"):
            continue
        cleaned = ("/v1" + token) if not token.startswith("/v1") else token
        cleaned = cleaned.rstrip(".,")
        if cleaned in {"/v1/commands", "/v1/..."} or cleaned.endswith("/..."):
            continue
        if cleaned not in paths:
            paths.append(cleaned)
    return paths


def parse_errors(text: str) -> List[Dict[str, str]]:
    """§15 error taxonomy: code, family and HTTP status."""
    errors: List[Dict[str, str]] = []
    for cells in _table_rows(_section(text, "## 15. Error taxonomy")):
        family = cells[0].strip()
        if not family or family.lower() == "family":
            continue
        status = ""
        status_match = re.search(r"\(([0-9]{3}(?:/[0-9]{3})?)\)", family)
        if status_match:
            status = status_match.group(1)
            family = family[: status_match.start()].strip()
        elif family.lower() == "generic":
            family = "Generic"
        codes = re.findall(r"`([A-Z][A-Z0-9_]+)`", cells[1]) if len(cells) > 1 else []
        for code in codes:
            errors.append({"code": code, "family": family, "http_status": status or _status_for(code)})
    return errors


def _status_for(code: str) -> str:
    if code == "NOT_FOUND":
        return "404"
    if code == "RATE_LIMITED":
        return "429"
    if code == "STREAM_BACKPRESSURE":
        return "WS close"
    if code == "INTERNAL":
        return "500"
    return ""


def parse_effect_classes(text: str) -> List[Dict[str, str]]:
    """§7.1 effect classes with tier, default policy and reconciliation strategy."""
    classes: List[Dict[str, str]] = []
    for cells in _table_rows(_section(text, "### 7.1 Effect classes")):
        match = re.fullmatch(r"`([a-z][a-z0-9\._]+)`(?: \(.*\))?", cells[0].strip())
        if not match or len(cells) < 4:
            continue
        classes.append(
            {
                "effect_class": match.group(1),
                "tier": cells[1].strip(),
                "default_policy": cells[2].strip(),
                "reconciliation": cells[3].strip(),
            }
        )
    return classes


def parse_event_families(text: str) -> List[str]:
    """§9.2 event families, e.g. `run.*`."""
    body = _section(text, "### 9.2 Event families")
    families: List[str] = []
    for token in re.findall(r"`([a-z][a-z_]*?)\.\*`", body):
        if token not in families:
            families.append(token)
    return families


def parse_trust_levels(text: str) -> List[str]:
    """§12 trust levels, in declaration order and without duplicates."""
    found = re.findall(
        r"`(TRUSTED_[A-Z_]+|VERIFIED_[A-Z_]+|AGENT_[A-Z_]+|UNTRUSTED_[A-Z_]+)`",
        _section(text, "## 12. Content trust"),
    )
    ordered: List[str] = []
    for level in found:
        if level not in ordered:
            ordered.append(level)
    return ordered


def parse_tool_names(text: str) -> List[str]:
    """§7.5 canonical Tool names (the `name (namespaced: ...)` list)."""
    body = _section(text, "### 7.5 Tool")
    start = body.find("name (namespaced:")
    if start == -1:
        return []
    end = body.find("), version", start)
    if end == -1:
        return []
    names = re.findall(r"([a-z][a-z0-9]*(?:\.[a-z0-9<>_]+)+)", body[start:end])
    ordered: List[str] = []
    for name in names:
        if name not in ordered:
            ordered.append(name)
    return ordered


def build_catalog(domain_path: Optional[Path] = None) -> Dict[str, object]:
    text = (domain_path or DOMAIN).read_text()
    return {
        "source": "DOMAIN.md",
        "glossary": parse_glossary(text),
        "ids": parse_id_prefixes(text),
        "commands": parse_commands(text),
        "read_projections": parse_read_projections(text),
        "errors": parse_errors(text),
        "effect_classes": parse_effect_classes(text),
        "event_families": parse_event_families(text),
        "trust_levels": parse_trust_levels(text),
        "tools": parse_tool_names(text),
    }


CATALOG_FILES = {
    "glossary": "glossary.yaml",
    "ids": "ids.yaml",
    "commands": "commands.yaml",
    "read_projections": "read-projections.yaml",
    "errors": "errors.yaml",
    "effect_classes": "effect-classes.yaml",
    "event_families": "event-families.yaml",
    "trust_levels": "trust-levels.yaml",
    "tools": "tools.yaml",
}

CATALOG_HEADER = (
    "# GENERATED from DOMAIN.md by scripts/ci/gen_contracts.py --catalog.\n"
    "# Do not hand edit: regenerate instead. DOMAIN.md is the naming/shape authority.\n"
)


def render_catalog(catalog: Dict[str, object], key: str) -> str:
    import yaml

    return CATALOG_HEADER + yaml.safe_dump({key: catalog[key]}, sort_keys=False, allow_unicode=True)


def write_catalog(out_dir: Optional[Path] = None, domain_path: Optional[Path] = None) -> List[Path]:
    out = out_dir or CATALOG_DIR
    out.mkdir(parents=True, exist_ok=True)
    catalog = build_catalog(domain_path)
    written: List[Path] = []
    for key, filename in CATALOG_FILES.items():
        path = out / filename
        path.write_text(render_catalog(catalog, key))
        written.append(path)
    return written
