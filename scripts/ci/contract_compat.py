#!/usr/bin/env python3
"""Protobuf compatibility and lint gate for Quansio contracts (GOV-004).

`DOMAIN.md` §17 requires additive-only changes within `v1`: a field number, type or
enum value that a released worker already understands must never change meaning. This
tool enforces that mechanically against a checked-in descriptor baseline:

  python3 scripts/ci/contract_compat.py --lint             schema lint + protoc compile
  python3 scripts/ci/contract_compat.py --compat           compare against the baseline
  python3 scripts/ci/contract_compat.py --update-baseline  record the current descriptors

Also used by tests/contract/: the baseline lives at
`tests/contract/compat/descriptor.baseline.pb`.

Python 3.11+ ; requires protoc and the `protobuf` runtime.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

ROOT = Path(__file__).resolve().parents[2]
PROTO_ROOT = ROOT / "schemas" / "proto"
BASELINE = ROOT / "tests" / "contract" / "compat" / "descriptor.baseline.pb"
PACKAGE_RE = re.compile(r"^quansio\.v1\.[a-z][a-z0-9_]*$")
ENUM_VALUE_RE = re.compile(r"^[A-Z][A-Z0-9_]*$")
MESSAGE_RE = re.compile(r"^message\s+([A-Za-z0-9_]+)\s*\{", re.MULTILINE)
ENUM_RE = re.compile(r"^enum\s+([A-Za-z0-9_]+)\s*\{", re.MULTILINE)


def proto_files(root: Optional[Path] = None) -> List[Path]:
    return sorted(((root or ROOT) / "schemas" / "proto").rglob("*.proto"))


def compile_descriptor_set(out: Path) -> Path:
    """Compile every proto into a FileDescriptorSet (also proves protoc accepts them)."""
    protos = [str(p.relative_to(PROTO_ROOT)) for p in proto_files()]
    if not protos:
        raise SystemExit("no .proto files found")
    out.parent.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        [
            "protoc",
            f"--proto_path={PROTO_ROOT}",
            f"--descriptor_set_out={out}",
            "--include_imports",
            *protos,
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"protoc failed:\n{result.stdout}\n{result.stderr}")
    return out


def load_descriptor_set(path: Path):
    from google.protobuf import descriptor_pb2

    descriptor_set = descriptor_pb2.FileDescriptorSet()
    descriptor_set.ParseFromString(path.read_bytes())
    return descriptor_set


def schema_lint(root: Optional[Path] = None, compile_protos: bool = True) -> List[str]:
    """Static lint over the proto sources, plus a real protoc compilation."""
    root = root or ROOT
    findings: List[str] = []
    for path in proto_files(root):
        rel = str(path.relative_to(root))
        text = path.read_text()
        package = re.search(r"^package\s+([A-Za-z0-9_.]+);", text, re.MULTILINE)
        if not package:
            findings.append(f"{rel}: missing package declaration")
        elif not PACKAGE_RE.match(package.group(1)):
            findings.append(f"{rel}: package must be quansio.v1.<area>, found {package.group(1)!r}")
        if 'syntax = "proto3";' not in text:
            findings.append(f"{rel}: must declare proto3 syntax")
        # Every message carries schema_version (DOMAIN.md §17).
        for name in MESSAGE_RE.findall(text):
            body = _block(text, f"message {name}")
            if "schema_version" not in body:
                findings.append(f"{rel}: message {name} must carry schema_version")
        for name in ENUM_RE.findall(text):
            body = _block(text, f"enum {name}")
            values = re.findall(r"^\s+([A-Z][A-Z0-9_]*)\s*=", body, re.MULTILINE)
            if not values:
                findings.append(f"{rel}: enum {name} has no values")
                continue
            if not values[0].endswith("_UNSPECIFIED"):
                findings.append(f"{rel}: enum {name} must start with a *_UNSPECIFIED value")
            for value in values:
                if not ENUM_VALUE_RE.match(value):
                    findings.append(f"{rel}: enum {name} value {value} must be SCREAMING_SNAKE_CASE")
            # Enums are prefixed: every value shares one prefix, including *_UNSPECIFIED.
            common = _common_value_prefix(values)
            if common is None:
                findings.append(
                    f"{rel}: enum {name} values must share one prefix (e.g. {_enum_prefix(name)}VALUE)"
                )
    if compile_protos:
        # protoc is the authoritative lint.
        compile_descriptor_set(Path("/dev/null"))
    return findings


def _block(text: str, header: str) -> str:
    start = text.index(header) + len(header)
    depth = 0
    out: List[str] = []
    for char in text[start:]:
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                break
        out.append(char)
    return "".join(out)


def _enum_prefix(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).upper() + "_"


def _common_value_prefix(values: Sequence[str]) -> Optional[str]:
    """Shared leading prefix of every enum value, or None when there is none.

    `_UNSPECIFIED` is normalised to the empty prefix so an enum whose zero value is the
    only one without a value-specific suffix still passes.
    """
    candidates = [v[: -len("UNSPECIFIED") - 1] if v.endswith("_UNSPECIFIED") else v + "_" for v in values]
    prefix = candidates[0]
    for candidate in candidates[1:]:
        while prefix and not candidate.startswith(prefix):
            prefix = prefix[: prefix.rfind("_")] if "_" in prefix[:-1] else prefix[:-1]
    return prefix.rstrip("_") + "_" if prefix.rstrip("_") else None


def descriptor_index(descriptor_set) -> Dict[str, Dict[str, Dict[int, Tuple[str, str]]]]:
    """package.message -> {field_number: (name, type)} for each message in the set."""
    index: Dict[str, Dict[str, Dict[int, Tuple[str, str]]]] = defaultdict(dict)
    for file in descriptor_set.file:
        for message in file.message_type:
            fields = {
                field.number: (field.name, str(field.type) + (field.type_name or ""))
                for field in message.field
            }
            index[file.package][f"{file.package}.{message.name}"] = fields
            for nested in message.nested_type:
                nested_fields = {
                    field.number: (field.name, str(field.type) + (field.type_name or ""))
                    for field in nested.field
                }
                index[file.package][f"{file.package}.{message.name}.{nested.name}"] = nested_fields
    return index


def enum_index(descriptor_set) -> Dict[str, Dict[int, str]]:
    index: Dict[str, Dict[int, str]] = defaultdict(dict)
    for file in descriptor_set.file:
        for enum in file.enum_type:
            index[file.package][f"{file.package}.{enum.name}"] = {
                value.number: value.name for value in enum.value
            }
        for message in file.message_type:
            for enum in message.enum_type:
                index[file.package][f"{file.package}.{message.name}.{enum.name}"] = {
                    value.number: value.name for value in enum.value
                }
    return index


def compare(current, baseline) -> List[str]:
    """Additive-only rules: no message/enum removal, no field/enum-number change."""
    findings: List[str] = []
    cur_messages = descriptor_index(current)
    base_messages = descriptor_index(baseline)
    for package, messages in base_messages.items():
        for message, fields in messages.items():
            if message not in cur_messages.get(package, {}):
                findings.append(f"{message}: message removed (v1 is additive-only)")
                continue
            current_fields = cur_messages[package][message]
            for number, (name, type_name) in fields.items():
                if number not in current_fields:
                    findings.append(f"{message}: field #{number} ({name}) removed")
                    continue
                cur_name, cur_type = current_fields[number]
                if cur_name != name:
                    findings.append(f"{message}: field #{number} renamed {name} -> {cur_name}")
                if cur_type != type_name:
                    findings.append(
                        f"{message}: field #{number} ({name}) type changed {type_name} -> {cur_type}"
                    )
    cur_enums = enum_index(current)
    base_enums = enum_index(baseline)
    for package, enums in base_enums.items():
        for enum, values in enums.items():
            if enum not in cur_enums.get(package, {}):
                findings.append(f"{enum}: enum removed (v1 is additive-only)")
                continue
            current_values = cur_enums[package][enum]
            for number, name in values.items():
                if number not in current_values:
                    findings.append(f"{enum}: value #{number} ({name}) removed")
                elif current_values[number] != name:
                    findings.append(
                        f"{enum}: value #{number} renamed {name} -> {current_values[number]}"
                    )
    return findings


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Contract lint and compatibility gate")
    ap.add_argument("--lint", action="store_true")
    ap.add_argument("--compat", action="store_true")
    ap.add_argument("--update-baseline", action="store_true")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    if args.update_baseline:
        compile_descriptor_set(BASELINE)
        print(json.dumps({"baseline": str(BASELINE.relative_to(ROOT))}))
        return 0

    findings = schema_lint()
    if args.compat or not args.lint:
        if not BASELINE.exists():
            findings.append(
                f"{BASELINE.relative_to(ROOT)} missing: record it with --update-baseline"
            )
        else:
            import tempfile

            with tempfile.TemporaryDirectory() as tmp:
                current = compile_descriptor_set(Path(tmp) / "current.pb")
                findings += compare(load_descriptor_set(current), load_descriptor_set(BASELINE))

    if args.json:
        print(json.dumps(findings, indent=2))
    elif not findings:
        print("contract check: CLEAN (lint + compatibility)")
    else:
        for finding in findings:
            print(" -", finding)
        print(f"contract check: {len(findings)} finding(s)")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
