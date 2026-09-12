#!/usr/bin/env python3
"""Generate Quansio contracts from DOMAIN.md and the Protobuf/JSON sources.

Derivation chain (DOMAIN.md is the naming/shape authority, DOSSIER.md §18):

    DOMAIN.md ──► schemas/catalog/*.yaml ──► schemas/openapi/public-api-v1.yaml
                                          └─► sdk/typescript/src/generated/public-api.d.ts
    schemas/proto/**/*.proto ──► crates/contracts/src/generated/*.rs        (prost, Rust)
                             └─► python/intelligence/contracts/generated/*  (protoc, Python)

Usage:
  python3 scripts/ci/gen_contracts.py --all        regenerate every generated artifact
  python3 scripts/ci/gen_contracts.py --catalog    regenerate the DOMAIN.md-derived catalogs
  python3 scripts/ci/gen_contracts.py --openapi    regenerate the public OpenAPI 3.1 document
  python3 scripts/ci/gen_contracts.py --rust       regenerate Rust bindings (prost)
  python3 scripts/ci/gen_contracts.py --python     regenerate Python bindings (protoc + grpc plugin)
  python3 scripts/ci/gen_contracts.py --ts         regenerate TypeScript types (openapi-typescript)
  python3 scripts/ci/gen_contracts.py --check      regenerate into a temp dir and diff (CI gate)

Python 3.11+ ; PyYAML.
"""
from __future__ import annotations

import argparse
import filecmp
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from scripts.ci import domain_catalog  # noqa: E402

SCHEMA_DIR = ROOT / "schemas"
PROTO_DIR = SCHEMA_DIR / "proto"
OPENAPI_PATH = SCHEMA_DIR / "openapi" / "public-api-v1.yaml"
TS_TYPES_PATH = ROOT / "sdk" / "typescript" / "src" / "generated" / "public-api.d.ts"
RUST_GENERATED = ROOT / "crates" / "contracts" / "src" / "generated"
PY_GENERATED = ROOT / "python" / "intelligence" / "contracts" / "generated"

API_VERSION = "v1"
HTTP_BY_FAMILY = {
    "Auth": "401",
    "Validation": "400",
    "Conflict": "409",
    "Authority": "403",
    "Runtime": "409",
    "Execution": "503",
    "Model": "502",
    "Generic": "500",
}


def proto_files() -> List[Path]:
    return sorted(PROTO_DIR.rglob("*.proto"))


def require(tool: str) -> None:
    if shutil.which(tool) is None:
        raise SystemExit(f"{tool} is required to generate contracts but was not found on PATH")


def generate_openapi(out: Path, catalog: Optional[Dict[str, object]] = None) -> Path:
    """Write the public API v1 OpenAPI 3.1 document derived from the DOMAIN.md catalog."""
    import yaml

    catalog = catalog or domain_catalog.build_catalog()
    commands: Dict[str, List[str]] = catalog["commands"]  # type: ignore[assignment]
    errors: List[Dict[str, str]] = catalog["errors"]  # type: ignore[assignment]
    projections: List[str] = catalog["read_projections"]  # type: ignore[assignment]

    error_schema = {
        "type": "object",
        "additionalProperties": False,
        "required": ["code", "message", "correlation_id", "retryable"],
        "properties": {
            "code": {
                "type": "string",
                "enum": [e["code"] for e in errors],
                "description": "DOMAIN.md §15 error taxonomy.",
            },
            "message": {"type": "string"},
            "correlation_id": {"type": "string"},
            "retryable": {"type": "boolean"},
            "details": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "properties": {"key": {"type": "string"}, "value": {"type": "string"}},
                },
            },
        },
    }

    schemas: Dict[str, object] = {
        "Error": error_schema,
        "CommandEnvelope": {
            "type": "object",
            "additionalProperties": False,
            "required": ["command_id"],
            "properties": {
                "command_id": {
                    "type": "string",
                    "description": "Client-generated ULID; idempotent per tenant (DOMAIN.md §1.2).",
                },
                "params": {"type": "object"},
            },
        },
    }

    paths: Dict[str, object] = {}
    for family, names in commands.items():
        for name in names:
            params_schema = f"{name}Params"
            schemas[params_schema] = {
                "type": "object",
                "additionalProperties": True,
                "description": (
                    f"{family} command {name}. The concrete parameter schema is owned by the "
                    "implementing task and must be added additively; until then this schema is "
                    "deliberately open and marked x-quansio-params-defined: false."
                ),
                "x-quansio-params-defined": False,
            }
            paths[f"/v1/commands/{name}"] = {
                "post": {
                    "operationId": name,
                    "summary": f"{family}: {name}",
                    "tags": [family],
                    "requestBody": {
                        "required": True,
                        "content": {
                            "application/json": {"schema": {"$ref": "#/components/schemas/CommandEnvelope"}}
                        },
                    },
                    "responses": {
                        "200": {
                            "description": "Accepted; result or typed error (DOMAIN.md §14).",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "required": ["command_id", "accepted_at"],
                                        "properties": {
                                            "command_id": {"type": "string"},
                                            "accepted_at": {"type": "string", "format": "date-time"},
                                            "result": {"type": ["object", "null"]},
                                            "error": {
                                                "oneOf": [
                                                    {"$ref": "#/components/schemas/Error"},
                                                    {"type": "null"},
                                                ]
                                            },
                                        },
                                    }
                                }
                            },
                        },
                        "default": {
                            "description": "Typed error (DOMAIN.md §15).",
                            "content": {
                                "application/json": {"schema": {"$ref": "#/components/schemas/Error"}}
                            },
                        },
                    },
                }
            }

    for path in projections:
        if path.endswith("/stream"):
            continue
        paths[path] = {
            "get": {
                "operationId": "Read" + "".join(part.title() for part in path.strip("/").split("/") if part),
                "summary": f"Read projection {path} (DOMAIN.md §14).",
                "tags": ["Read projections"],
                "parameters": [
                    {"name": "cursor", "in": "query", "schema": {"type": "string"}},
                    {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 500}},
                ],
                "responses": {
                    "200": {
                        "description": "Projection page with cursor.",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["items"],
                                    "properties": {
                                        "items": {"type": "array", "items": {"type": "object"}},
                                        "next_cursor": {"type": ["string", "null"]},
                                    },
                                }
                            }
                        },
                    },
                    "default": {
                        "description": "Typed error (DOMAIN.md §15).",
                        "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}},
                    },
                },
            }
        }

    document = {
        "openapi": "3.1.0",
        "info": {
            "title": "Quansio Public API",
            "version": API_VERSION,
            "description": (
                "Generated from DOMAIN.md by scripts/ci/gen_contracts.py. Commands are "
                "POST /v1/commands/<Name> with idempotent command_id; reads are GET projections. "
                "Additive changes only within v1 (DOMAIN.md §17). The WebSocket stream "
                "(GET /v1/stream) is described in DOMAIN.md §9.3 and is not an OpenAPI path."
            ),
        },
        "servers": [{"url": "https://{host}/v1", "variables": {"host": {"default": "localhost:8443"}}}],
        "security": [{"bearerAuth": []}],
        "tags": [{"name": family} for family in commands] + [{"name": "Read projections"}],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {"type": "http", "scheme": "bearer", "bearerFormat": "ES256 JWT"},
                "refreshCookie": {"type": "apiKey", "in": "cookie", "name": "q_refresh"},
            },
            "schemas": schemas,
        },
        "x-quansio-error-http-map": {e["code"]: e["http_status"] for e in errors},
        "x-quansio-event-families": catalog["event_families"],
        "x-quansio-effect-classes": [c["effect_class"] for c in catalog["effect_classes"]],  # type: ignore[index]
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(yaml.safe_dump(document, sort_keys=False, allow_unicode=True, width=120))
    return out


def generate_rust(out: Path) -> Path:
    require("cargo")
    result = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--manifest-path",
            "scripts/dev/contract-gen/Cargo.toml",
            "--",
            "--out",
            str(out),
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"rust binding generation failed:\n{result.stdout}\n{result.stderr}")
    return out


def generate_python(out: Path) -> Path:
    out.mkdir(parents=True, exist_ok=True)
    protos = [str(p.relative_to(PROTO_DIR)) for p in proto_files()]
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "grpc_tools.protoc",
            f"--proto_path={PROTO_DIR}",
            f"--python_out={out}",
            f"--grpc_python_out={out}",
            f"--pyi_out={out}",
            *protos,
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            "python binding generation failed (grpcio-tools is required):\n"
            f"{result.stdout}\n{result.stderr}"
        )
    # protoc does not emit package markers; add them so the tree is importable.
    for directory in sorted([out, *(d for d in out.rglob("*") if d.is_dir())]):
        init = directory / "__init__.py"
        if not init.exists():
            init.write_text(
                '"""Generated Python bindings for Quansio contracts. Do not hand edit."""\n'
            )
    return out


def generate_ts(out: Path) -> Path:
    require("pnpm")
    out.parent.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        ["pnpm", "dlx", "openapi-typescript", str(OPENAPI_PATH), "-o", str(out)],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"typescript generation failed:\n{result.stdout}\n{result.stderr}")
    return out


def _diff_tree(a: Path, b: Path) -> List[str]:
    """Relative names that differ between two generated trees."""
    if not a.exists() or not b.exists():
        return [f"{a} or {b} missing"]
    differing: List[str] = []
    ignored = ("__pycache__", ".pytest_cache")

    def files(root: Path) -> set[Path]:
        return {
            p.relative_to(root)
            for p in root.rglob("*")
            if p.is_file() and not any(part in ignored or part.endswith(".pyc") for part in p.parts)
        }

    files_a = files(a)
    files_b = files(b)
    for rel in sorted(files_a | files_b):
        pa, pb = a / rel, b / rel
        if not pa.exists() or not pb.exists() or not filecmp.cmp(pa, pb, shallow=False):
            differing.append(str(rel))
    return differing


GENERATORS = ("catalog", "openapi", "rust", "python", "ts")


def check(only: Sequence[str] = GENERATORS) -> List[str]:
    """Regenerate into a temp dir and report drift against the checked-in artifacts."""
    problems: List[str] = []
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        if "catalog" in only:
            out = tmp_path / "catalog"
            domain_catalog.write_catalog(out)
            problems += [f"catalog/{d}" for d in _diff_tree(out, domain_catalog.CATALOG_DIR)]
        if "openapi" in only:
            out = generate_openapi(tmp_path / "public-api-v1.yaml")
            if not OPENAPI_PATH.exists() or out.read_text() != OPENAPI_PATH.read_text():
                problems.append("openapi/public-api-v1.yaml")
        if "rust" in only:
            out = generate_rust(tmp_path / "rust")
            problems += [f"rust/{d}" for d in _diff_tree(out, RUST_GENERATED)]
        if "python" in only:
            out = generate_python(tmp_path / "python")
            problems += [f"python/{d}" for d in _diff_tree(out, PY_GENERATED)]
        if "ts" in only:
            out = generate_ts(tmp_path / "public-api.d.ts")
            if not TS_TYPES_PATH.exists() or out.read_text() != TS_TYPES_PATH.read_text():
                problems.append("ts/public-api.d.ts")
    return problems


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Generate Quansio contracts")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--catalog", action="store_true")
    ap.add_argument("--openapi", action="store_true")
    ap.add_argument("--rust", action="store_true")
    ap.add_argument("--python", action="store_true")
    ap.add_argument("--ts", action="store_true")
    ap.add_argument("--check", action="store_true", help="verify checked-in artifacts are current")
    ap.add_argument("--only", default=None, help="comma-separated subset for --check")
    args = ap.parse_args(argv)

    if args.check:
        only = args.only.split(",") if args.only else list(GENERATORS)
        problems = check(only)
        if problems:
            print("contract regeneration diff detected:")
            for problem in problems:
                print(" -", problem)
            print("run: python3 scripts/ci/gen_contracts.py --all")
            return 1
        print(f"contract regeneration check: CLEAN ({','.join(only)})")
        return 0

    run_all = args.all or not any([args.catalog, args.openapi, args.rust, args.python, args.ts])
    if run_all or args.catalog:
        for path in domain_catalog.write_catalog():
            print("wrote", path.relative_to(ROOT))
    if run_all or args.openapi:
        generate_openapi(OPENAPI_PATH)
        print("wrote", OPENAPI_PATH.relative_to(ROOT))
    if run_all or args.rust:
        generate_rust(RUST_GENERATED)
        print("wrote", RUST_GENERATED.relative_to(ROOT))
    if run_all or args.python:
        generate_python(PY_GENERATED)
        print("wrote", PY_GENERATED.relative_to(ROOT))
    if run_all or args.ts:
        generate_ts(TS_TYPES_PATH)
        print("wrote", TS_TYPES_PATH.relative_to(ROOT))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
