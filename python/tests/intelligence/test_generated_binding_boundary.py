"""Boundary-structure tests for the intelligence gateway (INT-001).

These tests pin the two architectural facts the service exists to guarantee:

  * the servicer speaks the *generated* `quansio.v1.intelligence.IntelligenceGateway`
    contract through the canonical `quansio.v1.*` import path — never the nested
    `intelligence.contracts.generated` alias — and its callable surface is exactly that
    contract;
  * nothing in the served Python exposes a way to mutate canonical runtime, graph, effect or
    machine state, and no provider SDK or store client is imported outside its owner.
"""

from __future__ import annotations

import ast
import importlib
import json
from pathlib import Path

from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.server.servicer import IMPLEMENTED_METHODS, IntelligenceGatewayServicer

PY_ROOT = Path(__file__).resolve().parents[2]
REPO_ROOT = Path(__file__).resolve().parents[3]
PLANE = PY_ROOT / "intelligence"
SERVER_PACKAGE = PLANE / "server"
TRUST_PACKAGE = PLANE / "trust"
GENERATED_ROOT = (PLANE / "contracts" / "generated").resolve()

# The alias protoc emits around the generated tree; the canonical import path is quansio.v1.*.
NESTED_ALIAS = "intelligence" + ".contracts.generated"

FORBIDDEN_IMPORT_ROOTS = {
    "psycopg",
    "psycopg2",
    "asyncpg",
    "sqlalchemy",
    "redis",
    "nats",
    "pymongo",
    "motor",
    "aiokafka",
    "kafka",
    "anthropic",
    "openai",
    "litellm",
    "mistralai",
    "cohere",
    "boto3",
    "vertexai",
}

FORBIDDEN_TOKENS = (
    "effect_ledger",
    "reserve_effect",
    "settle_effect",
    "ApprovalReceipt",
    "graph_transaction",
    "register_tool",
    "INSERT INTO",
    "UPDATE ",
    "DELETE FROM",
)

MUTATION_VERBS = ("write", "commit", "settle", "approve", "grant", "mutate", "execute", "dispatch")


def _module_sources(package: Path) -> list[Path]:
    return sorted(path for path in package.rglob("*.py"))


def _imported_modules(path: Path) -> set[str]:
    roots: set[str] = set()
    for node in ast.walk(ast.parse(path.read_text())):
        if isinstance(node, ast.Import):
            roots.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            roots.add(node.module)
    return roots


def test_servicer_surface_is_exactly_the_generated_contract() -> None:
    service = service_pb2.DESCRIPTOR.services_by_name["IntelligenceGateway"]
    generated = {method.name for method in service.methods}
    servicer = IntelligenceGatewayServicer()
    callables = {
        name for name in dir(servicer) if not name.startswith("_") and callable(getattr(servicer, name))
    }
    assert generated <= callables, "every generated RPC must be served"
    assert callables - generated == {"begin_drain", "wait_for_idle"}, (
        "the servicer must expose no method beyond the generated contract and call draining"
    )
    assert not {name for name in callables if any(verb in name.lower() for verb in MUTATION_VERBS)}


def test_implemented_and_unimplemented_partition_the_contract() -> None:
    service = service_pb2.DESCRIPTOR.services_by_name["IntelligenceGateway"]
    generated = {method.name for method in service.methods}
    owners = IntelligenceGatewayServicer.UNIMPLEMENTED_OWNERS
    assert generated - set(owners) == IMPLEMENTED_METHODS
    # INT-002 implemented model fulfillment, INT-011 the embedding request class and INT-007 the
    # memory proposal path; every other RPC still names its owning task.
    assert {"ClassifyTrust", "FulfillModel", "Embed", "ProposeMemory"} == IMPLEMENTED_METHODS
    assert set(owners) == generated - IMPLEMENTED_METHODS


def test_unimplemented_owners_are_registered_tasks() -> None:
    tasks = json.loads((REPO_ROOT / "registries" / "tasks.json").read_text())
    entries = tasks["tasks"] if isinstance(tasks, dict) else tasks
    known = {entry["id"] for entry in entries}
    missing = sorted(set(IntelligenceGatewayServicer.UNIMPLEMENTED_OWNERS.values()) - known)
    assert missing == [], f"unimplemented RPCs must name a registered task: {missing}"


def test_generated_bindings_resolve_through_the_canonical_import_path() -> None:
    generated_grpc = importlib.import_module("quansio.v1.intelligence.service_pb2_grpc")
    module_path = Path(generated_grpc.__file__).resolve()
    assert GENERATED_ROOT in module_path.parents
    assert module_path == GENERATED_ROOT / "quansio" / "v1" / "intelligence" / "service_pb2_grpc.py"
    assert generated_grpc.__name__ == "quansio.v1.intelligence.service_pb2_grpc"
    assert NESTED_ALIAS not in generated_grpc.__name__
    # The server registers the servicer with exactly this generated module.
    from intelligence.server import app

    assert app.service_pb2_grpc is generated_grpc
    assert service_pb2_grpc is generated_grpc


def test_intelligence_package_never_uses_the_nested_alias() -> None:
    offenders = [
        str(path.relative_to(REPO_ROOT))
        for path in PLANE.rglob("*.py")
        if "generated" not in path.parts and NESTED_ALIAS in path.read_text()
    ]
    assert offenders == [], f"use quansio.v1.* instead of the nested alias: {offenders}"


def test_served_modules_import_only_their_own_generated_contract() -> None:
    offenders: list[str] = []
    for path in (*_module_sources(SERVER_PACKAGE), *_module_sources(TRUST_PACKAGE)):
        for module in _imported_modules(path):
            root = module.split(".")[0]
            if root in FORBIDDEN_IMPORT_ROOTS:
                offenders.append(f"{path.name}: {module}")
            if module.startswith("quansio.") and not module.startswith("quansio.v1.intelligence"):
                offenders.append(f"{path.name}: {module}")
    assert offenders == [], offenders


def test_served_modules_contain_no_canonical_state_mutation_surface() -> None:
    offenders: list[str] = []
    for path in (*_module_sources(SERVER_PACKAGE), *_module_sources(TRUST_PACKAGE)):
        text = path.read_text()
        for token in FORBIDDEN_TOKENS:
            if token in text:
                offenders.append(f"{path.name}: {token}")
    assert offenders == [], offenders
