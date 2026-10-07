"""Tool visibility in the model gateway (ADP-008, DOMAIN.md §7.5).

The Rust Tool Registry decides who may propose a tool (`model_visible`, `host_callable`); the gateway's
`RegistryToolSchemaSource` is the one Python reader of `config/tools.yaml`. A tool that is not model-visible
is skipped on load, so it has no schema to offer: `definition()` answers `None` exactly as for a typo,
`resolve_tools` refuses the name, and no provider is ever sent it.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml

from intelligence.model_gateway import capabilities
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.tooling import (
    RegistryToolSchemaSource,
    default_tool_registry_path,
    resolve_tools,
)

HOST_ONLY = "webdev.preview_scopes"
MODEL_TOOLS = frozenset({capabilities.TOOLS})


def _declaration(name: str, extra: dict[str, object] | None = None) -> dict[str, object]:
    return {
        "name": name,
        "description": f"{name} fixture",
        "input_schema": {
            "type": "object",
            "additionalProperties": False,
            "properties": {"key": {"type": "string"}},
            "required": ["key"],
        },
        **(extra or {}),
    }


def _write(tmp_path: Path, tools: list[dict[str, object]]) -> Path:
    path = tmp_path / "tools.yaml"
    path.write_text(yaml.safe_dump({"version": 1, "tools": tools}), encoding="utf-8")
    return path


def test_the_shipped_source_holds_every_model_visible_tool_and_no_host_only_one() -> None:
    document = yaml.safe_load(default_tool_registry_path().read_text(encoding="utf-8"))
    declared = {entry["name"]: entry for entry in document["tools"]}
    assert declared[HOST_ONLY]["model_visible"] is False
    assert declared[HOST_ONLY]["host_callable"] is True
    source = RegistryToolSchemaSource.load()
    visible = {name for name, entry in declared.items() if entry.get("model_visible", True)}
    assert set(source.names()) == visible
    assert source.definition(HOST_ONLY) is None
    assert source.definition("webdev.preview_open") is not None


def test_a_host_only_tool_cannot_be_offered_to_a_provider() -> None:
    source = RegistryToolSchemaSource.load()
    # Offered alone, or among the model's own tools, the name is refused: a typo and a host-only tool look alike.
    for names in ([HOST_ONLY], ["fs.read", HOST_ONLY]):
        with pytest.raises(GatewayError) as error:
            resolve_tools(names, source, MODEL_TOOLS)
        assert error.value.code == GatewayErrorCode.VALIDATION_SCHEMA
        assert HOST_ONLY in str(error.value)
    assert [definition.name for definition in resolve_tools(["fs.read"], source, MODEL_TOOLS)] == ["fs.read"]


def test_model_visible_defaults_to_true_and_false_hides_the_declaration(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        [
            _declaration("note.read"),
            _declaration("note.edit", {"model_visible": True}),
            _declaration("panel.list", {"model_visible": False, "host_callable": True}),
        ],
    )
    source = RegistryToolSchemaSource.load(path)
    assert set(source.names()) == {"note.read", "note.edit"}
    assert source.definition("panel.list") is None


def test_a_flag_that_is_not_a_boolean_fails_the_load_instead_of_defaulting_to_visible(tmp_path: Path) -> None:
    for bad in ("no", 0, None, [], "false"):
        path = _write(tmp_path, [_declaration("panel.list", {"model_visible": bad})])
        with pytest.raises(GatewayError) as error:
            RegistryToolSchemaSource.load(path)
        assert error.value.code == GatewayErrorCode.INTERNAL, bad


def test_the_gateway_has_no_other_reader_of_the_registry() -> None:
    root = Path(__file__).resolve().parents[2] / "intelligence"
    readers = sorted(
        str(path.relative_to(root))
        for path in root.rglob("*.py")
        if "safe_load" in path.read_text(encoding="utf-8")
        and "tools.yaml" in path.read_text(encoding="utf-8")
    )
    assert readers == ["model_gateway/tooling.py"]
