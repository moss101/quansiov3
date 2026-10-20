"""A tool's picture reaches a model that can see it; one that cannot is told so (APP-007 / INT-002).

A screenshot of the page the agent is driving is the evidence a computer-using agent needs, and it is
untrusted page content like any other tool result. These tests assert the bytes each provider is sent:

* Anthropic carries the picture inside the `tool_result`; the chat-completions tool message is text-only, so
  OpenAI carries it in a user message that follows the run of tool messages and says whose picture it is;
* an *optional* picture sent to a model without `vision` is replaced by a sentence the model reads, and a
  *required* one still fails the call: nothing is dropped silently;
* a picture costs the context budget a fixed amount, not its base64 as if it were prose.
"""

from __future__ import annotations

import base64
import json
from collections.abc import Iterator

import pytest
from quansio.v1.intelligence import intelligence_pb2

from intelligence.context.turn import IMAGE_TOKENS, estimate_tokens
from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.adapters.base import OPTIONAL_IMAGE_OMITTED
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.conformance import (
    CONFORMANCE_TOOL_NAME,
    StubProvider,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.conformance.suite import CONFORMANCE_TOOL_CALL_ID, CONFORMANCE_TOOL_RESULT
from intelligence.model_gateway.content import parse_content
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.tooling import wire_tool_name

PNG_BASE64 = base64.b64encode(b"\x89PNG\r\n\x1a\n pixels of the page").decode("ascii")


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


def _call_with_picture(model_id: str, *, optional: bool) -> intelligence_pb2.ModelCallRequest:
    """The conformance tool round, with a screenshot beside the tool's text result."""
    call = build_call(model_catalog_id=model_id, scenario="tool_result", tools=True)
    result = call.messages[-1]
    assert result.role == intelligence_pb2.RenderedMessage.ROLE_TOOL
    picture: dict[str, object] = {"kind": "image", "media_type": "image/png", "data_base64": PNG_BASE64}
    if optional:
        picture["optional"] = True
    result.content_json = json.dumps(
        {
            "parts": [{"kind": "text", "text": CONFORMANCE_TOOL_RESULT}, picture],
            "tool_call_id": CONFORMANCE_TOOL_CALL_ID,
            "tool_name": CONFORMANCE_TOOL_NAME,
        }
    )
    return call


def test_only_an_image_may_be_optional() -> None:
    parsed = parse_content(
        json.dumps(
            {
                "parts": [
                    {"kind": "image", "media_type": "image/png", "data_base64": PNG_BASE64, "optional": True}
                ]
            }
        ),
        is_tool_message=False,
    )
    assert parsed.has_optional_image and not parsed.has_required_image
    for bad in (
        {"kind": "text", "text": "x", "optional": True},
        {"kind": "image", "media_type": "image/png", "data_base64": PNG_BASE64, "optional": "yes"},
    ):
        with pytest.raises(GatewayError) as excinfo:
            parse_content(json.dumps({"parts": [bad]}), is_tool_message=False)
        assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA


def test_anthropic_carries_the_picture_inside_the_tool_result(gateway: ModelGateway) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC)
    body = gateway.prepare(_call_with_picture(model_id, optional=True)).prepared_request.body_json()
    result = body["messages"][-1]["content"][0]
    assert result["type"] == "tool_result" and result["tool_use_id"] == CONFORMANCE_TOOL_CALL_ID
    assert [block["type"] for block in result["content"]] == ["text", "image"]
    assert result["content"][1]["source"] == {"type": "base64", "media_type": "image/png", "data": PNG_BASE64}


def test_openai_follows_the_tool_message_with_a_user_message_that_holds_the_picture(
    gateway: ModelGateway,
) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.OPENAI)
    body = gateway.prepare(_call_with_picture(model_id, optional=True)).prepared_request.body_json()
    messages = body["messages"]
    tool_index = next(index for index, message in enumerate(messages) if message["role"] == "tool")
    assert messages[tool_index]["content"] == CONFORMANCE_TOOL_RESULT, "the tool message stays text"
    after = messages[tool_index + 1]
    assert after["role"] == "user" and tool_index + 2 == len(messages)
    assert after["content"][0]["type"] == "text" and CONFORMANCE_TOOL_CALL_ID in after["content"][0]["text"]
    assert after["content"][1] == {
        "type": "image_url",
        "image_url": {"url": f"data:image/png;base64,{PNG_BASE64}"},
    }


def test_a_model_that_cannot_see_is_told_an_optional_picture_was_left_out_and_a_required_one_still_fails(
    gateway: ModelGateway,
) -> None:
    model_id = catalog_model_for_kind(gateway.catalog, ProviderKind.OPENAI_COMPATIBLE)
    body = gateway.prepare(_call_with_picture(model_id, optional=True)).prepared_request.body_json()
    rendered = json.dumps(body["messages"])
    assert PNG_BASE64 not in rendered and "image_url" not in rendered
    tool_message = next(message for message in body["messages"] if message["role"] == "tool")
    assert OPTIONAL_IMAGE_OMITTED in tool_message["content"], "the model reads what it was not shown"
    with pytest.raises(GatewayError) as excinfo:
        gateway.prepare(_call_with_picture(model_id, optional=False))
    assert excinfo.value.code is GatewayErrorCode.VALIDATION_SCHEMA and "vision" in excinfo.value.message


def test_a_picture_costs_the_budget_a_fixed_amount_whatever_its_bytes() -> None:
    text = json.dumps({"parts": [{"kind": "text", "text": "x" * 40}]})
    small = json.dumps(
        {
            "parts": [
                {"kind": "text", "text": "x" * 40},
                {"kind": "image", "media_type": "image/png", "data_base64": "A" * 400},
            ]
        }
    )
    large = json.dumps(
        {
            "parts": [
                {"kind": "text", "text": "x" * 40},
                {"kind": "image", "media_type": "image/png", "data_base64": "A" * 400_000},
            ]
        }
    )
    assert estimate_tokens(small) == estimate_tokens(large), "the bytes do not change the cost"
    assert estimate_tokens(small) - estimate_tokens(text) >= IMAGE_TOKENS
    assert estimate_tokens(large) < IMAGE_TOKENS + 200, "not the picture's base64 counted as prose"
    assert estimate_tokens(json.dumps("plain text of forty characters, no picture")) == 11


def test_registry_tool_names_are_still_encoded_for_the_wire() -> None:
    assert wire_tool_name(CONFORMANCE_TOOL_NAME) != ""
