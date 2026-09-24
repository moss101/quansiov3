"""Tool turns through the model gateway (INT-002, DOMAIN.md §7.4, §11.1).

A tool loop has two rounds: the model calls a tool, then the model answers from the result. The
second round only reaches a provider if the conversation carries the assistant turn that made
the call, and only with tool names the provider accepts. These tests pin the three pieces that
make that true, against the offline stub (which refuses what the first-party providers refuse):

* the rendered-content contract carries an assistant turn's tool calls, and nowhere else;
* registry names (`fs.read`) are encoded for the wire (`fs__read`) and decoded on the way back;
* the production gateway offers the Tool Registry's own schemas (`config/tools.yaml`).
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path

import pytest
import yaml
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway, RegistryToolSchemaSource
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.conformance import (
    StubProvider,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.conformance.suite import (
    CONFORMANCE_TOOL_CALL_ID,
    CONFORMANCE_TOOL_NAME,
    run_scenario,
)
from intelligence.model_gateway.content import parse_content
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.tooling import (
    default_tool_registry_path,
    registry_tool_name,
    resolve_tools,
    wire_tool_name,
)

KINDS = (ProviderKind.ANTHROPIC, ProviderKind.OPENAI, ProviderKind.OPENAI_COMPATIBLE)


@pytest.fixture
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


def _gateway(stub: StubProvider) -> ModelGateway:
    return ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ=credential_environ(),
        tool_schemas=conformance_tool_schemas(),
    )


def _registry_names() -> list[str]:
    document = yaml.safe_load(default_tool_registry_path().read_text(encoding="utf-8"))
    return [entry["name"] for entry in document["tools"]]


# -- the content contract ------------------------------------------------------------------


def test_an_assistant_turn_carries_its_tool_calls() -> None:
    content = parse_content(
        json.dumps(
            {
                "parts": [{"kind": "text", "text": "reading it"}],
                "tool_calls": [{"id": "c1", "name": "fs.read", "args": {"path": "/a"}}],
            }
        ),
        is_tool_message=False,
        is_assistant_message=True,
    )
    assert [(call.id, call.name, dict(call.args)) for call in content.tool_calls] == [
        ("c1", "fs.read", {"path": "/a"})
    ]
    assert content.tool_calls[0].args_json() == '{"path":"/a"}'


@pytest.mark.parametrize(
    ("payload", "assistant", "detail"),
    [
        ({"tool_calls": [{"id": "c1", "name": "fs.read", "args": {}}]}, False, "only an assistant"),
        ({"tool_calls": {"id": "c1"}}, True, "must be a list"),
        ({"tool_calls": [{"id": "", "name": "fs.read", "args": {}}]}, True, "non-empty"),
        ({"tool_calls": [{"id": "c1", "name": "fs.read", "args": "{}"}]}, True, "JSON object"),
        ({"tool_calls": [{"id": "c1", "name": "fs.read", "args": {}, "x": 1}]}, True, "unknown key"),
        (
            {"tool_calls": [{"id": "c1", "name": "a", "args": {}}, {"id": "c1", "name": "b", "args": {}}]},
            True,
            "repeats a tool call id",
        ),
    ],
)
def test_malformed_tool_calls_fail_closed(payload: dict[str, object], assistant: bool, detail: str) -> None:
    with pytest.raises(GatewayError) as refused:
        parse_content(json.dumps(payload), is_tool_message=False, is_assistant_message=assistant)
    assert refused.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert detail in str(refused.value)


# -- tool names on the wire ----------------------------------------------------------------


def test_every_registry_tool_name_round_trips_through_its_wire_spelling() -> None:
    names = _registry_names()
    assert "fs.read" in names and "artifact.create" in names
    for name in names:
        wire = wire_tool_name(name)
        assert all(character.isalnum() or character in "_-" for character in wire), wire
        assert len(wire) <= 64
        assert registry_tool_name(wire) == name


@pytest.mark.parametrize("name", ["fs__read", "Fs.Read", "fs..read", "connector.<id>.<op>", "", "a" * 65])
def test_a_name_the_wire_cannot_carry_reversibly_is_refused(name: str) -> None:
    with pytest.raises(GatewayError) as refused:
        wire_tool_name(name)
    assert refused.value.code is GatewayErrorCode.VALIDATION_SCHEMA


def test_an_invented_wire_name_decodes_to_a_name_the_registry_does_not_have() -> None:
    # Nothing here decides what is callable: the runtime rejects an unregistered tool.
    assert registry_tool_name("delete_everything") == "delete_everything"
    assert registry_tool_name("fs__nuke") == "fs.nuke"


@pytest.mark.parametrize("kind", KINDS, ids=lambda kind: kind.value)
def test_a_provider_tool_call_arrives_under_its_registry_name(stub: StubProvider, kind: ProviderKind) -> None:
    gateway = _gateway(stub)
    report = run_scenario(
        gateway, model_catalog_id=catalog_model_for_kind(gateway.catalog, kind), scenario="tool_call"
    )
    sent = json.loads(stub.state.calls[-1].body)
    offered = (
        sent["tools"][0]["name"] if kind is ProviderKind.ANTHROPIC else sent["tools"][0]["function"]["name"]
    )
    assert offered == "fs__read"
    (call,) = report.of_kind(intelligence_pb2.ModelEvent.KIND_TOOL_CALL)
    assert call.tool_name == CONFORMANCE_TOOL_NAME == "fs.read"


# -- the second round ----------------------------------------------------------------------


@pytest.mark.parametrize("kind", KINDS, ids=lambda kind: kind.value)
def test_the_second_round_carries_the_call_its_result_answers(stub: StubProvider, kind: ProviderKind) -> None:
    gateway = _gateway(stub)
    run_scenario(
        gateway, model_catalog_id=catalog_model_for_kind(gateway.catalog, kind), scenario="tool_result"
    )
    messages = json.loads(stub.state.calls[-1].body)["messages"]
    if kind is ProviderKind.ANTHROPIC:
        assistant, result = messages[-2], messages[-1]
        assert assistant["role"] == "assistant"
        assert assistant["content"] == [
            {"type": "tool_use", "id": CONFORMANCE_TOOL_CALL_ID, "name": "fs__read", "input": {"value": "42"}}
        ]
        assert result["role"] == "user"
        assert result["content"][0]["type"] == "tool_result"
        assert result["content"][0]["tool_use_id"] == CONFORMANCE_TOOL_CALL_ID
    else:
        assistant, result = messages[-2], messages[-1]
        assert assistant == {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {
                    "id": CONFORMANCE_TOOL_CALL_ID,
                    "type": "function",
                    "function": {"name": "fs__read", "arguments": '{"value":"42"}'},
                }
            ],
        }
        assert result["role"] == "tool" and result["tool_call_id"] == CONFORMANCE_TOOL_CALL_ID


@pytest.mark.parametrize("kind", KINDS, ids=lambda kind: kind.value)
def test_a_tool_result_without_its_call_is_refused_as_a_provider_would(
    stub: StubProvider, kind: ProviderKind
) -> None:
    """The stub holds the first-party rule, so the offline suite fails where a live run would."""
    gateway = _gateway(stub)
    call = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, kind), scenario="tool_result", tools=True
    )
    del call.messages[-2]  # drop the assistant turn that made the call
    events = list(gateway.fulfill(call))
    errors = [event for event in events if event.kind == intelligence_pb2.ModelEvent.KIND_ERROR]
    assert errors and errors[0].error.code == "VALIDATION_SCHEMA", events
    # The gateway sent it and the provider refused it (a gateway refusal would send nothing).
    assert len(stub.state.calls) == 1


# -- the production tool schemas -----------------------------------------------------------


def test_the_production_gateway_offers_the_registry_schemas(tmp_path: Path) -> None:
    source = RegistryToolSchemaSource.load()
    assert set(source.names()) == set(_registry_names())
    resolved = resolve_tools(source.names(), source, frozenset({"tools"}))
    assert [tool.name for tool in resolved] == list(source.names())
    registry = yaml.safe_load(default_tool_registry_path().read_text(encoding="utf-8"))
    by_name = {entry["name"]: entry for entry in registry["tools"]}
    for tool in resolved:
        assert tool.strict_input_schema() == by_name[tool.name]["input_schema"]
    # Reserved families are not declarations, so a model is never offered one.
    for family in ("connector.<id>.<op>", "scm.git.<op>", "scm.pr.<op>"):
        assert source.definition(family) is None

    catalog = tmp_path / "models.yaml"
    catalog.write_text(
        "version: 1\n"
        "providers:\n  openai-compatible:\n    kind: openai_compatible\n"
        "    base_url: http://127.0.0.1:9/v1\n    credential_handle: provider/tool-turns\n"
        "routing:\n  strategy: deterministic\n  primary: local\n  fallback_order: []\n"
        "models:\n  - id: local\n    provider: openai-compatible\n    model_id: tool-turns-stub\n"
        "    capabilities: [tools, streaming]\n    context_window: 32768\n    cost_class: low\n"
        "    dlp_eligible: false\n",
        encoding="utf-8",
    )
    gateway = ModelGateway.from_environment(
        catalog_path=catalog, environ={"QUANSIO_SECRET_PROVIDER_TOOL_TURNS": "x"}
    )
    call = build_call(model_catalog_id="local", scenario="text")
    call.tools.append("fs.read")
    body = gateway.prepare(call).prepared_request.body_json()
    assert body["tools"][0]["function"]["name"] == "fs__read"
    assert body["tools"][0]["function"]["parameters"] == by_name["fs.read"]["input_schema"]
    call.tools.append("scm.git.<op>")
    with pytest.raises(GatewayError) as refused:
        gateway.prepare(call)
    assert refused.value.code is GatewayErrorCode.VALIDATION_SCHEMA


def test_a_registry_that_cannot_be_read_fails_closed(tmp_path: Path) -> None:
    broken = tmp_path / "tools.yaml"
    broken.write_text("tools: {not: a list}\n", encoding="utf-8")
    with pytest.raises(GatewayError) as refused:
        RegistryToolSchemaSource.load(broken)
    assert refused.value.code is GatewayErrorCode.INTERNAL
    with pytest.raises(GatewayError):
        RegistryToolSchemaSource.load(tmp_path / "missing.yaml")
