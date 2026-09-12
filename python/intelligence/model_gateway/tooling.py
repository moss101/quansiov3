"""Strict tool schemas for model tool calling (DOMAIN.md §7.5, §11.1).

The Tool Registry is Rust runtime authority (RUN-011/D-014); `ModelCallRequest.tools` carries
the filtered tool *names* the model may see. The gateway never invents a schema for a name: it
resolves each name through an injected read-only `ToolSchemaSource` and fails closed when a
name has no registered strict schema, so a model is never offered a tool whose contract the
runtime cannot validate.

`strict` means what DOMAIN.md §7.4 requires at the runtime boundary: an object schema with
`additionalProperties: false`. Adapters send that shape with the provider's own strictness
switch where the provider has one.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from typing import Protocol

from intelligence.model_gateway import capabilities
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode


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
        seen.add(name)
        resolved.append(definition)
    return tuple(resolved)
