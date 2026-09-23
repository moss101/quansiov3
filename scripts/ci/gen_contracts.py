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
# The SDK operation tables (APP-013): the same catalog, as data the thin clients address.
SDK_TS_OPERATIONS = ROOT / "sdk" / "typescript" / "src" / "generated" / "operations.ts"
SDK_PY_OPERATIONS = ROOT / "sdk" / "python" / "quansio_sdk" / "generated" / "operations.py"
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


# Parameter schemas the implementing task has defined (additive over the open default).
DEFINED_PARAMS: Dict[str, Dict[str, object]] = {
    "CreateWebhookSubscription": {
        "type": "object",
        "required": ["url"],
        "properties": {
            "url": {"type": "string", "maxLength": 2048, "description": "Absolute http(s) URL; its host and port are the grant (DOMAIN.md §13.5)."},
            "workspace_id": {"type": "string", "description": "Scope; tenant-wide without one (ManageTenant)."},
            "event_types": {"type": "array", "maxItems": 64, "items": {"type": "string"}, "description": "`family.name` or `family.*`; empty selects every type."},
        },
    },
    "UpdateWebhookSubscription": {
        "type": "object",
        "required": ["subscription_id"],
        "properties": {
            "subscription_id": {"type": "string"},
            "status": {"type": "string", "enum": ["active", "paused"]},
            "event_types": {"type": "array", "maxItems": 64, "items": {"type": "string"}},
            "secret": {"type": "string", "enum": ["rotate"], "description": "Replace the signing secret; the result carries the new one once."},
        },
    },
    "RedeliverWebhook": {
        "type": "object",
        "required": ["delivery_id"],
        "properties": {"delivery_id": {"type": "string"}},
    },
    "DeleteWebhookSubscription": {
        "type": "object",
        "required": ["subscription_id"],
        "properties": {"subscription_id": {"type": "string"}},
    },
}


def _runtime_event_schema() -> Dict[str, object]:
    """The RuntimeEvent envelope JSON Schema (schemas/json), as an OpenAPI component."""
    schema = json.loads((SCHEMA_DIR / "json" / "runtime-event.schema.json").read_text())
    for key in ("$schema", "$id"):
        schema.pop(key, None)
    return schema


def _stream_paths(catalog: Dict[str, object]) -> Dict[str, object]:
    """`GET /v1/stream` (WebSocket) and `GET /v1/stream/events` (SSE), DOMAIN.md §9.3."""
    error = {"description": "Typed error (DOMAIN.md §15).", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}
    return {
        "/v1/stream": {
            "get": {
                "operationId": "Stream",
                "summary": "The event stream over WebSocket (DOMAIN.md §9.3).",
                "description": (
                    "Upgrade to a WebSocket, then send one StreamSubscribe message. The server sends "
                    "StreamEventFrame (durable, replayable, with a cursor) and StreamLiveFrame (transient) "
                    "messages; reconnect with the last cursor to replay what was missed. A slow consumer is "
                    "disconnected with STREAM_BACKPRESSURE. A protected thread's events reach only its participants."
                ),
                "tags": ["Stream"],
                "parameters": [{"name": "x-quansio-consumer", "in": "header", "schema": {"type": "string"}}],
                "responses": {"101": {"description": "Switching Protocols to WebSocket."}, "default": error},
                "x-quansio-transport": "websocket",
                "x-quansio-client-message": {"$ref": "#/components/schemas/StreamSubscribe"},
                "x-quansio-server-messages": [
                    {"$ref": "#/components/schemas/StreamEventFrame"},
                    {"$ref": "#/components/schemas/StreamLiveFrame"},
                ],
            }
        },
        "/v1/stream/events": {
            "get": {
                "operationId": "StreamEvents",
                "summary": "The event stream as Server-Sent Events, for read-only consumers (DOMAIN.md §9.3).",
                "description": (
                    "Each durable event is one SSE message: `event: event`, `id:` the stream cursor, `data:` a "
                    "StreamEventFrame. Reconnecting with `Last-Event-ID` (or `cursor`) resumes after it. The same "
                    "channels and the same protected-thread gate as the WebSocket."
                ),
                "tags": ["Stream"],
                "parameters": [
                    {"name": "channel", "in": "query", "required": True, "style": "form", "explode": True,
                     "schema": {"type": "array", "items": {"type": "string", "pattern": "^(workspace|thread|run|target):[a-z]+_[0-9A-HJKMNP-TV-Z]{26}$"}}},
                    {"name": "cursor", "in": "query", "schema": {"type": "string"}},
                    {"name": "consumer", "in": "query", "schema": {"type": "string"}},
                    {"name": "Last-Event-ID", "in": "header", "schema": {"type": "string"}},
                ],
                "responses": {
                    "200": {
                        "description": "An event stream; each `data:` is a StreamEventFrame.",
                        "content": {"text/event-stream": {"schema": {"type": "string"}, "x-quansio-sse-data": {"$ref": "#/components/schemas/StreamEventFrame"}}},
                    },
                    "default": error,
                },
            }
        },
    }


def _webhooks(catalog: Dict[str, object]) -> Dict[str, object]:
    """The outbound webhook request (OpenAPI 3.1 `webhooks`), DOMAIN.md §13.5."""
    contract: Dict[str, object] = catalog.get("webhooks") or {}  # type: ignore[assignment]
    headers: List[str] = list(contract.get("headers", []))  # type: ignore[arg-type]
    described = {
        "quansio-event-id": "The stable event id; deliveries are at-least-once, so dedupe on it.",
        "quansio-event-type": "The event type.",
        "quansio-delivery-id": "The delivery (`whd_…`); the same across retries of one event.",
        "quansio-signature": (
            f"`t=<unix seconds>,{contract.get('signature_scheme', 'v1')}=<hex {contract.get('algorithm', 'HMAC-SHA256')}"
            f"(secret, \"{contract.get('signed_payload', '<t>.<body>')}\")>`; reject a timestamp more than "
            f"{contract.get('tolerance_seconds', 300)} s from your clock."
        ),
    }
    return {
        "runtimeEvent": {
            "post": {
                "operationId": "WebhookRuntimeEvent",
                "summary": "One RuntimeEvent, redacted, signed, delivered at least once (DOMAIN.md §13.5).",
                "parameters": [
                    {"name": name, "in": "header", "required": True, "schema": {"type": "string"}, "description": described.get(name, "")}
                    for name in headers
                ],
                "requestBody": {"required": True, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/RuntimeEvent"}}}},
                "responses": {
                    "2XX": {"description": "Acknowledged. Anything else, or no answer within 10 s, is retried with backoff."},
                },
            }
        }
    }


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
            if name in DEFINED_PARAMS:
                schemas[params_schema] = {
                    **DEFINED_PARAMS[name],
                    "description": f"{family} command {name}.",
                    "x-quansio-params-defined": True,
                }
            else:
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

    schemas["RuntimeEvent"] = _runtime_event_schema()
    schemas["StreamSubscribe"] = {
        "type": "object",
        "required": ["subscribe"],
        "properties": {
            "subscribe": {"type": "array", "items": {"type": "object", "required": ["channel"], "properties": {"channel": {"type": "string"}}}},
            "cursor": {"type": "string"},
            "consumer": {"type": "string"},
        },
    }
    schemas["StreamEventFrame"] = {
        "type": "object",
        "required": ["kind", "cursor", "event"],
        "properties": {
            "kind": {"const": "event"},
            "cursor": {"type": "string", "description": "Opaque; resume after this event."},
            "event": {"$ref": "#/components/schemas/RuntimeEvent"},
        },
    }
    schemas["StreamLiveFrame"] = {
        "type": "object",
        "required": ["kind", "channel", "frame"],
        "properties": {
            "kind": {"const": "live"},
            "channel": {"type": "string"},
            "frame": {"type": "object", "description": "Transient (model.delta, terminal.bytes, browser.frame, progress); never persisted."},
        },
    }
    paths.update(_stream_paths(catalog))
    for path in projections:
        if path in paths:
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

    auth_endpoints: List[Dict[str, object]] = catalog.get("auth_endpoints", [])  # type: ignore[assignment]
    schemas["Session"] = {
        "type": "object",
        "additionalProperties": False,
        "required": ["session_id", "user_id", "tenant_id", "access_token", "access_expires_at", "refresh_token", "refresh_expires_at"],
        "properties": {
            "session_id": {"type": "string"},
            "user_id": {"type": "string"},
            "tenant_id": {"type": "string"},
            "access_token": {"type": "string", "description": "ES256 JWT, at most 15 minutes (DOMAIN.md §2)."},
            "access_expires_at": {"type": "string", "format": "date-time"},
            "refresh_token": {"type": "string", "description": "Opaque, rotating; stored in the OS keychain on desktop."},
            "refresh_expires_at": {"type": "string", "format": "date-time"},
            "workspace_id": {"type": "string"},
            "onboarded": {"type": "boolean"},
        },
    }
    for endpoint in auth_endpoints:
        path = str(endpoint["path"])
        method = str(endpoint["method"]).lower()
        operation_id = "".join(
            piece.title() for part in path.replace("/v1/", "").split("/") if part for piece in part.strip("{}").split("-")
        )
        operation_id = operation_id if operation_id.startswith("Auth") else "Auth" + operation_id
        operation: Dict[str, object] = {
            "operationId": operation_id,
            "summary": f"Session endpoint {endpoint['method']} {path} (DOMAIN.md §2).",
            "tags": ["Identity"],
            "security": [{"bearerAuth": []}] if endpoint["bearer"] else [],
            "responses": {
                "200": {"description": "Session, identity or acknowledgement.", "content": {"application/json": {"schema": {"type": "object"}}}},
                "default": {"description": "Typed error (DOMAIN.md §15).", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}},
            },
        }
        if "{provider}" in path:
            operation["parameters"] = [{"name": "provider", "in": "path", "required": True, "schema": {"type": "string", "enum": ["google", "microsoft", "github"]}}]
        if method == "post":
            operation["requestBody"] = {"required": True, "content": {"application/json": {"schema": {"type": "object", "additionalProperties": True}}}}
        paths.setdefault(path, {})[method] = operation  # type: ignore[index]

    document = {
        "openapi": "3.1.0",
        "info": {
            "title": "Quansio Public API",
            "version": API_VERSION,
            "description": (
                "Generated from DOMAIN.md by scripts/ci/gen_contracts.py. Commands are "
                "POST /v1/commands/<Name> with idempotent command_id; reads are GET projections. "
                "Additive changes only within v1 (DOMAIN.md §17). The WebSocket stream "
                "(GET /v1/stream) and its Server-Sent Events form (GET /v1/stream/events) are "
                "described below (DOMAIN.md §9.3); outbound webhooks are under `webhooks` (§13.5)."
            ),
        },
        "servers": [{"url": "https://{host}/v1", "variables": {"host": {"default": "localhost:8443"}}}],
        "security": [{"bearerAuth": []}],
        "tags": [{"name": family} for family in commands] + [{"name": "Read projections"}, {"name": "Stream"}],
        "paths": paths,
        "webhooks": _webhooks(catalog),
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
        "x-quansio-webhook-signature": catalog.get("webhooks") or {},
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


def _sdk_operations(catalog: Optional[Dict[str, object]] = None) -> Dict[str, object]:
    """The operation table both SDKs are generated from: one row per command, plus the
    read projections and the error codes, all from the DOMAIN.md catalog."""
    catalog = catalog or domain_catalog.build_catalog()
    commands: Dict[str, List[str]] = catalog["commands"]  # type: ignore[assignment]
    errors: List[Dict[str, str]] = catalog["errors"]  # type: ignore[assignment]
    projections: List[str] = catalog["read_projections"]  # type: ignore[assignment]
    operations = [
        {"operation_id": name, "method": "POST", "path": f"/v1/commands/{name}", "family": family}
        for family, names in commands.items()
        for name in names
    ]
    return {
        "operations": operations,
        "read_projections": list(projections),
        "error_codes": [e["code"] for e in errors],
        "webhook_signature": catalog.get("webhooks") or {},
    }


def generate_sdk_ts(out: Path, catalog: Optional[Dict[str, object]] = None) -> Path:
    """Write `operations.ts`: the command table, projections and error codes as `as const` data."""
    table = _sdk_operations(catalog)
    out.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "// GENERATED from DOMAIN.md by scripts/ci/gen_contracts.py --sdk. Do not hand edit.",
        "// The public API v1 operation table (APP-013): every command the catalog names, with",
        "// its method and path, the read projections, and the §15 error codes.",
        "",
        "export const OPERATIONS = {",
    ]
    for op in table["operations"]:  # type: ignore[union-attr]
        lines.append(
            f'  {op["operation_id"]}: {{ method: "{op["method"]}", path: "{op["path"]}", family: {json.dumps(op["family"])} }},'
        )
    lines += ["} as const;", "", "export type OperationId = keyof typeof OPERATIONS;", "", "export const READ_PROJECTIONS = ["]
    for path in table["read_projections"]:  # type: ignore[union-attr]
        lines.append(f"  {json.dumps(path)},")
    lines += ["] as const;", "", "export const ERROR_CODES = ["]
    for code in table["error_codes"]:  # type: ignore[union-attr]
        lines.append(f"  {json.dumps(code)},")
    lines += ["] as const;", "", "export type ErrorCode = (typeof ERROR_CODES)[number];", ""]
    signature: Dict[str, object] = table["webhook_signature"]  # type: ignore[assignment]
    lines += [
        "/** The outbound webhook request contract (DOMAIN.md §13.5). */",
        "export const WEBHOOK_SIGNATURE = {",
        f'  header: {json.dumps(signature.get("signature_header", ""))},',
        f'  headers: {json.dumps(signature.get("headers", []))},',
        f'  scheme: {json.dumps(signature.get("signature_scheme", ""))},',
        f'  signedPayload: {json.dumps(signature.get("signed_payload", ""))},',
        f'  algorithm: {json.dumps(signature.get("algorithm", ""))},',
        f'  toleranceSeconds: {int(signature.get("tolerance_seconds", 0))},',  # type: ignore[arg-type]
        f'  secretPrefix: {json.dumps(signature.get("secret_prefix", ""))},',
        "} as const;",
        "",
    ]
    out.write_text("\n".join(lines), encoding="utf-8")
    return out


def generate_sdk_py(out: Path, catalog: Optional[Dict[str, object]] = None) -> Path:
    """Write `operations.py`: the same table for the Python client."""
    table = _sdk_operations(catalog)
    out.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        '"""GENERATED from DOMAIN.md by scripts/ci/gen_contracts.py --sdk. Do not hand edit.',
        "",
        "The public API v1 operation table (APP-013): every command the catalog names, with its",
        "method and path, the read projections, and the §15 error codes.",
        '"""',
        "",
        "from __future__ import annotations",
        "",
        "OPERATIONS: dict[str, dict[str, str]] = {",
    ]
    for op in table["operations"]:  # type: ignore[union-attr]
        # One key per line, the shape ruff formats to, so the file is stable under both
        # the drift gate and the formatter.
        lines += [
            f'    {json.dumps(op["operation_id"])}: {{',
            f'        "method": "{op["method"]}",',
            f'        "path": "{op["path"]}",',
            f'        "family": {json.dumps(op["family"])},',
            "    },",
        ]
    lines += ["}", "", "READ_PROJECTIONS: tuple[str, ...] = ("]
    for path in table["read_projections"]:  # type: ignore[union-attr]
        lines.append(f"    {json.dumps(path)},")
    lines += [")", "", "ERROR_CODES: tuple[str, ...] = ("]
    for code in table["error_codes"]:  # type: ignore[union-attr]
        lines.append(f"    {json.dumps(code)},")
    signature: Dict[str, object] = table["webhook_signature"]  # type: ignore[assignment]
    lines += [")", "", "# The outbound webhook request contract (DOMAIN.md §13.5)."]
    lines.append(f'WEBHOOK_SIGNATURE_HEADER = {json.dumps(signature.get("signature_header", ""))}')
    lines.append("WEBHOOK_HEADERS: tuple[str, ...] = (")
    for header in signature.get("headers", []):  # type: ignore[union-attr]
        lines.append(f"    {json.dumps(header)},")
    lines.append(")")
    lines.append(f'WEBHOOK_SIGNATURE_SCHEME = {json.dumps(signature.get("signature_scheme", ""))}')
    lines.append(f'WEBHOOK_SIGNED_PAYLOAD = {json.dumps(signature.get("signed_payload", ""))}')
    lines.append(f'WEBHOOK_SIGNATURE_ALGORITHM = {json.dumps(signature.get("algorithm", ""))}')
    lines.append(f'WEBHOOK_TOLERANCE_SECONDS = {int(signature.get("tolerance_seconds", 0))}')  # type: ignore[arg-type]
    lines.append(f'WEBHOOK_SECRET_PREFIX = {json.dumps(signature.get("secret_prefix", ""))}')
    lines.append("")
    out.write_text("\n".join(lines), encoding="utf-8")
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


GENERATORS = ("catalog", "openapi", "rust", "python", "ts", "sdk")


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
        if "sdk" in only:
            out = generate_sdk_ts(tmp_path / "operations.ts")
            if not SDK_TS_OPERATIONS.exists() or out.read_text() != SDK_TS_OPERATIONS.read_text():
                problems.append("sdk/typescript/operations.ts")
            out = generate_sdk_py(tmp_path / "operations.py")
            if not SDK_PY_OPERATIONS.exists() or out.read_text() != SDK_PY_OPERATIONS.read_text():
                problems.append("sdk/python/operations.py")
    return problems


def main(argv: Optional[Sequence[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Generate Quansio contracts")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--catalog", action="store_true")
    ap.add_argument("--openapi", action="store_true")
    ap.add_argument("--rust", action="store_true")
    ap.add_argument("--python", action="store_true")
    ap.add_argument("--ts", action="store_true")
    ap.add_argument("--sdk", action="store_true", help="regenerate the SDK operation tables (APP-013)")
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

    run_all = args.all or not any([args.catalog, args.openapi, args.rust, args.python, args.ts, args.sdk])
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
    if run_all or args.sdk:
        generate_sdk_ts(SDK_TS_OPERATIONS)
        print("wrote", SDK_TS_OPERATIONS.relative_to(ROOT))
        generate_sdk_py(SDK_PY_OPERATIONS)
        print("wrote", SDK_PY_OPERATIONS.relative_to(ROOT))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
