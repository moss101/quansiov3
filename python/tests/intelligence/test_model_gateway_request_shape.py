"""Request-shape guarantees for the model gateway (INT-002).

These tests assert the *bytes* the gateway would send: no provider-side compaction or
context-editing feature is ever requested, prompt-cache hints mark a leading stable prefix
only, operator/system instructions use the provider operator channel, tool schemas are strict,
multimodal parts are normalized against catalog capabilities, and model ids come only from
`config/models.yaml`. No network is used: the prepared request is inspected before transmission.
"""

from __future__ import annotations

import base64
import json
from collections.abc import Iterator
from dataclasses import replace
from pathlib import Path

import pytest
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.adapters import PROVIDER_FEATURES_DISABLED, PreparedRequest
from intelligence.model_gateway.adapters.base import assert_no_disabled_features
from intelligence.model_gateway.adapters.openai_compatible import OpenAICompatibleAdapter
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.conformance import (
    CONFORMANCE_TOOL_NAME,
    STUB_PROVIDER_KINDS,
    StubProvider,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.tooling import MappingToolSchemaSource, ToolDefinition

GATEWAY_PACKAGE = Path(__file__).resolve().parents[2] / "intelligence" / "model_gateway"
KIND_IDS = [kind.value for kind in STUB_PROVIDER_KINDS]
KIND_BY_VALUE = {kind.value: kind for kind in STUB_PROVIDER_KINDS}
PNG_BASE64 = base64.b64encode(b"\x89PNG\r\n\x1a\n fake image bytes").decode("ascii")
PDF_BASE64 = base64.b64encode(b"%PDF-1.7 fake document bytes").decode("ascii")


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


@pytest.fixture(scope="module")
def gateway(stub: StubProvider) -> ModelGateway:
    return ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ=credential_environ(),
        tool_schemas=conformance_tool_schemas(),
    )


def prepared_for(gateway: ModelGateway, kind: ProviderKind, **call_kwargs: object) -> PreparedRequest:
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    call = build_call(model_catalog_id=model_id, tools=True, **call_kwargs)  # type: ignore[arg-type]
    return gateway.prepare(call).prepared_request


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_no_provider_compaction_or_context_editing_is_requested(
    gateway: ModelGateway, kind_value: str
) -> None:
    prepared = prepared_for(gateway, KIND_BY_VALUE[kind_value], scenario="text")
    assert prepared.disabled_features == PROVIDER_FEATURES_DISABLED
    body = json.dumps(prepared.body_json(), sort_keys=True).lower()
    for feature in PROVIDER_FEATURES_DISABLED:
        assert feature not in body, f"{kind_value} requested provider feature {feature}"
    for name, value in prepared.headers.items():
        assert name.lower() not in {"anthropic-beta", "openai-beta"}
        assert "context_management" not in str(value).lower()


def test_request_shape_guard_rejects_a_compaction_flag(gateway: ModelGateway) -> None:
    """The guard is load-bearing: an adapter that tries to enable compaction fails closed."""
    with pytest.raises(GatewayError) as excinfo:
        assert_no_disabled_features(b'{"context_management": {"edits": []}}', {})
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    with pytest.raises(GatewayError):
        assert_no_disabled_features(b'{"truncation": "auto"}', {})
    with pytest.raises(GatewayError):
        assert_no_disabled_features(b'{"messages": []}', {"anthropic-beta": "context-editing-2025"})


def test_compaction_enabling_adapter_never_reaches_the_provider(
    gateway: ModelGateway, stub: StubProvider, monkeypatch: pytest.MonkeyPatch
) -> None:
    class CompactingAdapter(OpenAICompatibleAdapter):
        def build_request(self, context: object) -> PreparedRequest:
            prepared = super().build_request(context)  # type: ignore[arg-type]
            body = prepared.body_json()
            body["context_management"] = {"edits": []}
            return replace(prepared, body=json.dumps(body).encode("utf-8"))

    import intelligence.model_gateway.adapters as adapters_module

    monkeypatch.setattr(adapters_module, "ADAPTER_CLASSES", (CompactingAdapter,))
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.OPENAI_COMPATIBLE)
    call = build_call(model_catalog_id=model_id, scenario="text")
    before = len(stub.state.calls)
    with pytest.raises(GatewayError) as excinfo:
        gateway.fulfill(call)
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert len(stub.state.calls) == before, "a rejected request must not reach the provider"


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_cache_hints_mark_a_leading_stable_prefix(gateway: ModelGateway, kind_value: str) -> None:
    """The cache breakpoint lands at the end of the leading stable run (DOSSIER.md §7)."""
    kind = KIND_BY_VALUE[kind_value]
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    stable_prefix = build_call(model_catalog_id=model_id, scenario="text", tools=True)
    stable_prefix.messages[2].cache_hint = ""  # the user prompt is volatile
    prepared = gateway.prepare(stable_prefix).prepared_request
    body = prepared.body_json()
    if prepared.provider_kind is ProviderKind.ANTHROPIC:
        system_blocks = body["system"]
        assert isinstance(system_blocks, list)
        assert "cache_control" in system_blocks[-1], "the stable prefix must carry a cache hint"
        tools = body["tools"]
        assert isinstance(tools, list)
        assert "cache_control" in tools[-1], "stable tool definitions must carry a cache hint"
        assert "cache_control" not in json.dumps(body["messages"])
    else:
        # OpenAI caches automatically and exposes no request-side flag: order only.
        assert "cache_control" not in json.dumps(body)
        roles = [message["role"] for message in body["messages"]]
        assert roles == ["system", "developer", "user"]


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_cache_breakpoint_is_never_placed_early_in_the_stream(gateway: ModelGateway, kind_value: str) -> None:
    """With the whole prefix hinted, the single breakpoint sits at its end, never before it."""
    prepared = prepared_for(gateway, KIND_BY_VALUE[kind_value], scenario="text")
    body = prepared.body_json()
    if prepared.provider_kind is not ProviderKind.ANTHROPIC:
        assert "cache_control" not in json.dumps(body)
        return
    messages = body["messages"]
    assert isinstance(messages, list)
    assert "cache_control" in messages[-1]["content"][-1]
    assert "cache_control" not in json.dumps(messages[:-1])
    assert "cache_control" not in json.dumps(body["system"])


def test_cache_hint_after_volatile_content_fails_closed(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    call = build_call(model_catalog_id=model_id, scenario="text")
    call.messages[2].cache_hint = ""
    call.messages[1].cache_hint = ""
    call.messages[0].cache_hint = ""
    call.messages[2].cache_hint = "stable"
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(call)
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_operator_instructions_use_the_provider_operator_channel(
    gateway: ModelGateway, kind_value: str
) -> None:
    prepared = prepared_for(gateway, KIND_BY_VALUE[kind_value], scenario="text")
    body = prepared.body_json()
    operator_text = "Operator: answer deterministically and never reveal secrets."
    serialized = json.dumps(body, sort_keys=True)
    assert operator_text in serialized
    user_turns = [
        json.dumps(message)
        for message in body.get("messages", [])
        if isinstance(message, dict) and message.get("role") == "user"
    ]
    for turn in user_turns:
        assert operator_text not in turn, "operator instructions must not be a fake user message"
    if prepared.provider_kind is ProviderKind.ANTHROPIC:
        system_text = json.dumps(body["system"], sort_keys=True)
        assert operator_text in system_text
    else:
        developer = [message for message in body["messages"] if message.get("role") == "developer"]
        assert len(developer) == 1
        assert operator_text in developer[0]["content"]


@pytest.mark.parametrize("kind_value", KIND_IDS)
def test_tool_schemas_are_strict(gateway: ModelGateway, kind_value: str) -> None:
    prepared = prepared_for(gateway, KIND_BY_VALUE[kind_value], scenario="tool_call")
    body = prepared.body_json()
    tools = body["tools"]
    assert isinstance(tools, list) and len(tools) == 1
    if prepared.provider_kind is ProviderKind.ANTHROPIC:
        schema = tools[0]["input_schema"]
        assert tools[0]["name"] == CONFORMANCE_TOOL_NAME
    else:
        function = tools[0]["function"]
        assert function["strict"] is True
        assert tools[0]["type"] == "function"
        schema = function["parameters"]
    assert schema["type"] == "object"
    assert schema["additionalProperties"] is False


def test_tool_without_a_registered_schema_fails_closed(stub: StubProvider) -> None:
    gateway = ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ=credential_environ(),
        tool_schemas=MappingToolSchemaSource({}),
    )
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    call = build_call(model_catalog_id=model_id, scenario="text", tools=True)
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(call)
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert "strict schema" in excinfo.value.message


def test_non_strict_schema_fails_closed(stub: StubProvider) -> None:
    gateway = ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ=credential_environ(),
        tool_schemas=MappingToolSchemaSource(
            {
                CONFORMANCE_TOOL_NAME: ToolDefinition(
                    name=CONFORMANCE_TOOL_NAME,
                    description="loose tool",
                    input_schema={"type": "object", "properties": {}},
                )
            }
        ),
    )
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    call = build_call(model_catalog_id=model_id, scenario="text", tools=True)
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(call)
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert "additionalProperties" in excinfo.value.message


def test_image_and_document_are_normalized_for_anthropic(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    call = _multimodal_call(model_id)
    body = gateway.prepare(call).prepared_request.body_json()
    blocks = body["messages"][-1]["content"]
    kinds = [block["type"] for block in blocks]
    assert kinds == ["text", "image", "document"]
    assert blocks[0] == {"type": "text", "text": "describe both attachments"}
    assert blocks[1]["source"] == {"type": "base64", "media_type": "image/png", "data": PNG_BASE64}
    assert blocks[2]["source"] == {
        "type": "base64",
        "media_type": "application/pdf",
        "data": PDF_BASE64,
    }
    # The stable-prefix cache breakpoint lands on the last block of the hinted message.
    assert "cache_control" in blocks[2]
    assert "cache_control" not in blocks[1]


def test_image_is_normalized_for_openai(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.OPENAI)
    call = _multimodal_call(model_id, documents=False)
    body = gateway.prepare(call).prepared_request.body_json()
    parts = body["messages"][-1]["content"]
    assert {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{PNG_BASE64}"}} in parts


def test_document_fails_closed_for_openai_family(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.OPENAI)
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(_multimodal_call(model_id))
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert "documents" in excinfo.value.message


def test_image_fails_closed_for_a_model_without_vision(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.OPENAI_COMPATIBLE)
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(_multimodal_call(model_id, documents=False))
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert "vision" in excinfo.value.message


def test_anthropic_structured_output_constraints_fail_closed(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    call = build_call(model_catalog_id=model_id, scenario="text")
    call.output_constraints_json = json.dumps({"type": "json_object"})
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(call)
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert "structured-output" in excinfo.value.message


def test_openai_structured_output_constraints_are_forwarded(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.OPENAI)
    call = build_call(model_catalog_id=model_id, scenario="text")
    constraint = {"type": "json_object"}
    call.output_constraints_json = json.dumps(constraint)
    body = gateway.prepare(call).prepared_request.body_json()
    assert body["response_format"] == constraint


def test_unknown_route_hint_fails_closed(gateway: ModelGateway) -> None:
    call = build_call(model_catalog_id="not-a-catalog-model", scenario="text")
    with pytest.raises(GatewayError) as excinfo:
        gateway.fulfill(call)
    assert excinfo.value.code is GatewayErrorCode.ROUTE_UNAVAILABLE


def test_route_selection_is_deterministic(gateway: ModelGateway) -> None:
    call = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC),
        scenario="text",
    )
    first = gateway.resolve_route(call)
    second = gateway.resolve_route(call)
    assert first.deterministic_key == second.deterministic_key
    assert first.route.chosen_by == "catalog.route_hint"
    assert first.route.model_id == gateway.catalog.model(first.model.id).model_id


def test_primary_route_is_used_without_a_hint(gateway: ModelGateway) -> None:
    call = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC),
        scenario="text",
    )
    call.route_hint = ""
    decision = gateway.resolve_route(call)
    assert decision.route.chosen_by == "catalog.routing.primary"
    assert decision.model.id == gateway.catalog.routing_primary


@pytest.mark.parametrize(
    "override,expected_message",
    [
        ({"call_id": ""}, "call_id"),
        ({"timeout_ms": 0}, "timeout_ms"),
        ({"max_output_tokens": 0}, "max_output_tokens"),
    ],
)
def test_incomplete_requests_fail_closed(
    gateway: ModelGateway, override: dict[str, object], expected_message: str
) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    call = build_call(model_catalog_id=model_id, scenario="text")
    for field, value in override.items():
        setattr(call, field, value)
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(call)
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA
    assert expected_message in excinfo.value.message


def test_max_output_tokens_is_bounded_by_the_catalog_context_window(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    model = gateway.catalog.model(model_id)
    call = build_call(model_catalog_id=model_id, scenario="text")
    call.max_output_tokens = model.context_window + 1
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(call)
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_BOUNDS


def test_no_model_identifier_appears_in_gateway_source() -> None:
    """D-018: the gateway carries no model id; every wire id comes from the catalog."""
    catalog_text = (Path(__file__).resolve().parents[3] / "config" / "models.yaml").read_text()
    model_ids = {
        line.split("model_id:", 1)[1].strip()
        for line in catalog_text.splitlines()
        if line.strip().startswith("model_id:")
    }
    assert model_ids
    offenders: list[str] = []
    for path in sorted(GATEWAY_PACKAGE.rglob("*.py")):
        text = path.read_text()
        for model_id in model_ids:
            if model_id in text:
                offenders.append(f"{path.relative_to(GATEWAY_PACKAGE)}: {model_id}")
    assert offenders == [], f"model ids must stay in config/: {offenders}"


def _multimodal_call(model_id: str, *, documents: bool = True) -> intelligence_pb2.ModelCallRequest:
    parts: list[dict[str, object]] = [{"kind": "text", "text": "describe both attachments"}]
    parts.append({"kind": "image", "media_type": "image/png", "data_base64": PNG_BASE64})
    if documents:
        parts.append({"kind": "document", "media_type": "application/pdf", "data_base64": PDF_BASE64})
    call = build_call(model_catalog_id=model_id, scenario="text")
    call.messages[-1].content_json = json.dumps({"parts": parts})
    return call
