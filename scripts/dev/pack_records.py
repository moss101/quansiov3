"""Built-in pack provenance digests (CAP-007).

A built-in pack's governance record (OPS-007) names the content it approved by digest, so an
edit that was not re-reviewed is visible:

* a skill pack's record lives in ``meta.yaml`` (``quarantine_record.provenance.digest``) and
  covers ``skill.yaml`` — sha256 of its bytes;
* a capability pack's record is inline in ``pack.yaml`` and covers the pack's content — sha256
  of the canonical JSON (sorted keys, no whitespace) of the pack without its governance
  sections and without ``provenance.digest`` itself.

``--check`` lists stale digests (exit 1 when any); ``--write`` refreshes the digest and the
review/normalization/evaluation/approval timestamps of the stale ones — to be run by the
governing process after it has re-reviewed and re-evaluated the change (the pack tests run the
evals). Run with ``uv run --project python python scripts/dev/pack_records.py``.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
SKILLS = ROOT / "packs" / "skills"
CAPABILITIES = ROOT / "packs" / "capabilities"
GOVERNANCE = ("quarantine", "review", "normalization", "evaluation", "approved")
_DIGEST = re.compile(r"^(\s*digest:\s*)sha256:[0-9a-f]{64}\s*$", re.MULTILINE)
_REVIEWED = re.compile(
    r'^(\s*(?:reviewed_at|normalized_at|evaluated_at|approved_at):\s*)"[^"]*"\s*$', re.MULTILINE
)


def skill_digest(skill_dir: Path) -> str:
    """The digest a skill's record must name: sha256 of ``skill.yaml``."""
    return "sha256:" + hashlib.sha256((skill_dir / "skill.yaml").read_bytes()).hexdigest()


def pack_digest(pack_path: Path) -> str:
    """The digest a capability pack's record must name: its content without the record."""
    pack = yaml.safe_load(pack_path.read_text(encoding="utf-8"))
    content = {key: value for key, value in pack.items() if key not in GOVERNANCE}
    provenance = dict(content.get("provenance") or {})
    provenance.pop("digest", None)
    content["provenance"] = provenance
    canonical = json.dumps(content, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return "sha256:" + hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def recorded_skill_digest(skill_dir: Path) -> str:
    meta = yaml.safe_load((skill_dir / "meta.yaml").read_text(encoding="utf-8"))
    return str(meta["quarantine_record"]["provenance"]["digest"])


def recorded_pack_digest(pack_path: Path) -> str:
    pack = yaml.safe_load(pack_path.read_text(encoding="utf-8"))
    return str(pack["provenance"]["digest"])


def records() -> list[tuple[str, Path, str, str]]:
    """``(name, record file, recorded digest, actual digest)`` for every built-in pack."""
    out = []
    for skill_dir in sorted(p for p in SKILLS.iterdir() if (p / "skill.yaml").is_file()):
        recorded = recorded_skill_digest(skill_dir)
        out.append((f"skill:{skill_dir.name}", skill_dir / "meta.yaml", recorded, skill_digest(skill_dir)))
    for pack_dir in sorted(p for p in CAPABILITIES.iterdir() if (p / "pack.yaml").is_file()):
        pack_path = pack_dir / "pack.yaml"
        out.append(
            (f"pack:{pack_dir.name}", pack_path, recorded_pack_digest(pack_path), pack_digest(pack_path))
        )
    return out


def refresh(record: Path, digest: str, at: str) -> None:
    text = record.read_text(encoding="utf-8")
    text, digests = _DIGEST.subn(lambda m: f"{m.group(1)}{digest}", text)
    if digests != 1:
        raise SystemExit(f"{record}: expected one provenance digest, found {digests}")
    text = _REVIEWED.sub(lambda m: f'{m.group(1)}"{at}"', text)
    record.write_text(text, encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--write", action="store_true", help="refresh stale records")
    args = parser.parse_args(argv)
    stale = [entry for entry in records() if entry[2] != entry[3]]
    if not stale:
        print("pack records: every digest names its content")
        return 0
    at = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    for name, record, recorded, actual in stale:
        if args.write:
            refresh(record, actual, at)
            print(f"refreshed {name}: {actual}")
        else:
            print(f"stale {name}: records {recorded}, content is {actual}")
    if args.write:
        # A pack's own digest excludes its record, so refreshing it cannot make it stale again.
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
