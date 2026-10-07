"""Strict tool schemas for model tool calling (DOMAIN.md §7.5, §11.1).

The Tool Registry is Rust runtime authority (RUN-011/D-014); `ModelCallRequest.tools` carries
the filtered tool *names* the model may see. The gateway never invents a schema for a name: it
resolves each name through an injected read-only `ToolSchemaSource` and fails closed when a
name has no registered strict schema, so a model is never offered a tool whose contract the
runtime cannot validate.

`strict` means what DOMAIN.md §7.4 requires at the runtime boundary: an object schema with
`additionalProperties: false`. Adapters send that shape with the provider's own strictness
switch where the provider has one.

The production source is `RegistryToolSchemaSource`: a read-only view of the same
`config/tools.yaml` the Rust Tool Registry loads, so the schema a model is offered is the schema
the runtime validates the proposal against, and a name the registry does not declare (a reserved
family, a typo) has no schema here either.

It holds only the **model-visible** declarations (ADP-008). A declaration with `model_visible: false`
is a host-only tool: it is skipped on load, so `definition()` answers `None` for it exactly as for a
typo, `resolve_tools` refuses the name, and no provider is ever sent its schema. `model_visible`
must be a boolean when present; anything else fails the load (the registry fails closed too).

Registry names are namespaced with dots (`fs.read`); both first-party wire formats accept only
`[A-Za-z0-9_-]` in a tool name. `wire_tool_name` encodes a registry name for the wire (`fs__read`)
and `registry_tool_name` decodes what a provider returns, so a proposal always names the registry
tool. A registry name the encoding cannot carry reversibly is refused rather than guessed at.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

import yaml

from intelligence.model_gateway import capabilities
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

TOOL_REGISTRY_RELATIVE_PATH = Path("config") / "tools.yaml"

#: What a registry name may contain; the separator is the only character the wire cannot carry.
_REGISTRY_NAME = re.compile(r"^[a-z0-9][a-z0-9_-]*(\.[a-z0-9][a-z0-9_-]*)*$")
_WIRE_SEPARATOR = "__"
#: The longest tool name both first-party wire formats accept.
MAX_WIRE_TOOL_NAME = 64


def wire_tool_name(name: str) -> str:
    """The provider-safe spelling of a registry tool name (`fs.read` -> `fs__read`)."""
    if not _REGISTRY_NAME.match(name) or _WIRE_SEPARATOR in name:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"tool name {name!r} cannot be encoded for a provider reversibly",
        )
    wire = name.replace(".", _WIRE_SEPARATOR)
    if len(wire) > MAX_WIRE_TOOL_NAME:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"tool name {name!r} is longer than a provider accepts ({MAX_WIRE_TOOL_NAME})",
        )
    return wire


def registry_tool_name(wire: str) -> str:
    """The registry name a provider's tool name stands for (`fs__read` -> `fs.read`).

    A name the model invented decodes to a name the registry does not have, and the runtime
    rejects the proposal as an unregistered tool; nothing here decides what is callable.
    """
    return wire.replace(_WIRE_SEPARATOR, ".")


@dataclass(frozen=True, slots=True)
class ToolDefinition:
    """A tool the model may call: name, description and a strict object input schema."""

    name: str
    description: str
    input_schema: Mapping[str, object]

    def strict_input_schema(self) -> dict[str, object]:
        """The schema as sent to providers, after proving it is strict."""
        schema = dict(self.input_schema)
        if schema.get("type") != "object":
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"tool '{self.name}' input schema must have type 'object'",
            )
        if schema.get("additionalProperties") is not False:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"tool '{self.name}' input schema must set additionalProperties=false",
            )
        properties = schema.get("properties", {})
        if not isinstance(properties, dict):
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"tool '{self.name}' input schema properties must be an object",
            )
        return schema


class ToolSchemaSource(Protocol):
    """Read-only lookup of registered tool definitions (supplied by the composition root)."""

    def definition(self, name: str) -> ToolDefinition | None: ...


class MappingToolSchemaSource:
    """`ToolSchemaSource` over an in-process mapping of registered tool definitions."""

    __slots__ = ("_definitions",)

    def __init__(self, definitions: Mapping[str, ToolDefinition] | None = None) -> None:
        self._definitions = dict(definitions or {})

    def definition(self, name: str) -> ToolDefinition | None:
        return self._definitions.get(name)


class RegistryToolSchemaSource:
    """`ToolSchemaSource` over the Tool Registry declarations in `config/tools.yaml` (read-only)."""

    __slots__ = ("_definitions",)

    def __init__(self, definitions: Mapping[str, ToolDefinition]) -> None:
        self._definitions = dict(definitions)

    @classmethod
    def load(cls, path: Path | None = None) -> RegistryToolSchemaSource:
        """Load the registry from `path` or the repository's `config/tools.yaml`."""
        resolved = path if path is not None else default_tool_registry_path()
        try:
            document = yaml.safe_load(resolved.read_text(encoding="utf-8"))
        except (OSError, yaml.YAMLError) as error:
            raise GatewayError(
                GatewayErrorCode.INTERNAL, f"tool registry {resolved} cannot be read"
            ) from error
        entries = document.get("tools") if isinstance(document, dict) else None
        if not isinstance(entries, list):
            raise GatewayError(GatewayErrorCode.INTERNAL, f"tool registry {resolved} declares no tools")
        definitions: dict[str, ToolDefinition] = {}
        for entry in entries:
            if not isinstance(entry, dict):
                continue
            name, schema = entry.get("name"), entry.get("input_schema")
            if not isinstance(name, str) or not isinstance(schema, dict):
                raise GatewayError(
                    GatewayErrorCode.INTERNAL, f"tool registry {resolved} has a malformed declaration"
                )
            visible = entry.get("model_visible", True)
            if not isinstance(visible, bool):
                raise GatewayError(
                    GatewayErrorCode.INTERNAL,
                    f"tool registry {resolved}: {name} has a model_visible that is not a boolean",
                )
            if not visible:
                continue
            definitions[name] = ToolDefinition(
                name=name, description=str(entry.get("description", "")), input_schema=schema
            )
        return cls(definitions)

    def definition(self, name: str) -> ToolDefinition | None:
        return self._definitions.get(name)

    def names(self) -> tuple[str, ...]:
        return tuple(self._definitions)


def default_tool_registry_path() -> Path:
    """Locate `config/tools.yaml` by walking up from this module to the repository root."""
    for parent in Path(__file__).resolve().parents:
        candidate = parent / TOOL_REGISTRY_RELATIVE_PATH
        if candidate.is_file():
            return candidate
    raise GatewayError(
        GatewayErrorCode.INTERNAL,
        f"{TOOL_REGISTRY_RELATIVE_PATH.as_posix()} not found above {Path(__file__).resolve()}",
    )


def resolve_tools(
    names: Iterable[str],
    source: ToolSchemaSource,
    model_capabilities: frozenset[str],
) -> tuple[ToolDefinition, ...]:
    """Strict definitions for the requested tool names, in request order, without duplicates."""
    requested = [name for name in names if name]
    if requested and capabilities.TOOLS not in model_capabilities:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            "tools were requested for a model that does not declare the `tools` capability",
        )
    resolved: list[ToolDefinition] = []
    seen: set[str] = set()
    for name in requested:
        if name in seen:
            continue
        definition = source.definition(name)
        if definition is None:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"tool '{name}' has no registered strict schema",
            )
        definition.strict_input_schema()
        wire_tool_name(name)
        seen.add(name)
        resolved.append(definition)
    return tuple(resolved)
