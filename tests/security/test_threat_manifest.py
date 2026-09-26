"""QA-007: the threat manifest covers DOSSIER §16 and the task's build list, with attacks that exist.

`tests/security/threats.yaml` is the minimum attack list made executable: every row of the §16 threat
table (parsed from DOSSIER.md, so a new threat without an attack fails here) and every QA-007 build item
names at least one attack, every attack names a test function that exists in the file it names, and no
P0/P1 finding is open.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = yaml.safe_load(
    (ROOT / "tests" / "security" / "threats.yaml").read_text(encoding="utf-8")
)


def dossier_threats() -> list[str]:
    text = (ROOT / "DOSSIER.md").read_text(encoding="utf-8")
    section = text.split("## 16. Security architecture", 1)[1].split("\n## ", 1)[0]
    rows = []
    for line in section.splitlines():
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if (
            len(cells) == 2
            and cells[0] not in ("Threat", "")
            and not set(cells[0]) <= {"-", " "}
        ):
            rows.append(cells[0])
    return rows


def attack_exists(reference: str) -> bool:
    path, _, name = reference.partition("::")
    source = ROOT / path
    if not name or not source.is_file():
        return False
    text = source.read_text(encoding="utf-8")
    pattern = (
        rf"^\s*(async\s+)?fn\s+{re.escape(name)}\s*\("
        if path.endswith(".rs")
        else rf"^def\s+{re.escape(name)}\s*\("
    )
    return re.search(pattern, text, flags=re.MULTILINE) is not None


def test_every_dossier_threat_has_an_attack() -> None:
    threats = dossier_threats()
    assert len(threats) >= 10, threats
    covered = {entry["threat"]: entry["attacks"] for entry in MANIFEST["threats"]}
    missing = [threat for threat in threats if not covered.get(threat)]
    assert not missing, f"DOSSIER §16 threats without an attack: {missing}"
    assert set(covered) == set(threats), (
        f"manifest rows not in DOSSIER §16: {set(covered) - set(threats)}"
    )


def test_every_build_item_has_an_attack() -> None:
    task = next(
        item
        for item in json.loads((ROOT / "registries" / "tasks.json").read_text())[
            "tasks"
        ]
        if item["id"] == "QA-007"
    )
    build = " ".join(task["build"]).lower()
    items = {entry["item"]: entry["attacks"] for entry in MANIFEST["build_items"]}
    for named in (
        "tenant breakout",
        "capability escalation",
        "approval substitution",
        "secret exfiltration",
        "ssrf/dns rebinding",
        "path traversal",
        "malicious skill/content",
        "unsafe preview",
        "stale lease reuse",
    ):
        assert named in build, f"{named!r} is no longer in QA-007's build list"
        assert any(key.lower() == named and value for key, value in items.items()), (
            named
        )
    assert "int-012 injection corpus" in build and items.get("INT-012 injection corpus")


def test_every_attack_names_a_test_that_exists() -> None:
    references = [ref for entry in MANIFEST["threats"] for ref in entry["attacks"]]
    references += [ref for entry in MANIFEST["build_items"] for ref in entry["attacks"]]
    references += [
        entry["regression"] for entry in MANIFEST["findings"] if entry.get("regression")
    ]
    missing = sorted({ref for ref in references if not attack_exists(ref)})
    assert not missing, missing


def test_no_p0_or_p1_finding_is_open() -> None:
    findings = MANIFEST["findings"]
    assert findings, "the register records what the campaign found"
    for finding in findings:
        assert finding["severity"] in ("P0", "P1", "P2", "P3"), finding
        assert finding["status"] in ("open", "fixed"), finding
        if finding["severity"] in ("P0", "P1"):
            assert finding["status"] == "fixed", finding
            assert finding.get("regression"), (
                f"{finding['id']} is fixed without a regression test"
            )
    ids = [finding["id"] for finding in findings]
    assert len(ids) == len(set(ids))
