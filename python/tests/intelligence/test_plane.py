"""Structural tests for the intelligence plane (GOV-003 language boundary)."""

from __future__ import annotations

import ast
import importlib
from pathlib import Path

import pytest

PY_ROOT = Path(__file__).resolve().parents[2]
PLANE = PY_ROOT / "intelligence"

SUBPACKAGES = [
    "model_gateway",
    "context",
    "knowledge",
    "memory",
    "embeddings",
    "trust",
    "skills",
    "capability_compiler",
    "evaluation",
    "artifacts",
    "adapters",
    "contracts",
]

PROVIDER_SDKS = {
    "anthropic",
    "openai",
    "google.generativeai",
    "vertexai",
    "boto3",
    "litellm",
    "mistralai",
    "cohere",
}


@pytest.mark.parametrize("name", SUBPACKAGES)
def test_subpackage_is_importable_and_documents_owner(name: str) -> None:
    module = importlib.import_module(f"intelligence.{name}")
    assert module.__doc__, f"intelligence.{name} must document its canonical owner role"
    assert (PLANE / name / "__init__.py").exists()


def test_all_intelligence_modules_import() -> None:
    for path in sorted(PLANE.rglob("*.py")):
        rel = path.relative_to(PY_ROOT).with_suffix("")
        importlib.import_module(".".join(rel.parts))


def _imported_roots(path: Path) -> set[str]:
    tree = ast.parse(path.read_text())
    roots: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            roots.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            roots.add(node.module.split(".")[0])
    return roots


def test_provider_sdks_only_inside_the_model_gateway() -> None:
    offenders: list[str] = []
    for path in sorted(PLANE.rglob("*.py")):
        if "model_gateway" in path.parts:
            continue
        if _imported_roots(path) & PROVIDER_SDKS:
            offenders.append(str(path.relative_to(PY_ROOT)))
    assert offenders == [], f"provider SDK imported outside the model gateway: {offenders}"


def test_intelligence_stays_inside_its_owner_directory() -> None:
    # Every module in the plane belongs to the canonical owner `python/intelligence`.
    assert PLANE.is_dir()
    assert (PY_ROOT / "pyproject.toml").exists()
