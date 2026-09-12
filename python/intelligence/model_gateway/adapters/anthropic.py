"""Anthropic Messages adapter (the primary provider, OD-004).

Wire reference: `POST {base_url}/v1/messages` with `stream: true`. The model id comes from the
catalog (`ModelSpec.model_id`); nothing here names a model.

Channel mapping (DOMAIN.md §11.1):

* `ROLE_SYSTEM` and `ROLE_OPERATOR` messages become top-level `system` blocks — Anthropic's
  operator channel — including mid-conversation operator instructions. They are never turned
  into fake user turns.
* `ROLE_TOOL` messages become `tool_result` blocks inside a user turn, which is the only
  place the Messages API accepts them.
* Prompt-cache hints become `cache_control: {"type": "ephemeral"}` on the last tool
  definition and on the final hinted message, so the stable prefix (system, tools, policy) is
  the cached region and volatile content follows it.
* Provider-side context management / compaction is not requested; the request-shape guard in
  `adapters.base` proves it before transmission.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from typing import Any

from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import capabilities
from intelligence.model_gateway.adapters.base import (
    ASSISTANT_ROLE,
    OPERATOR_ROLE,
    PROVIDER_FEATURES_DISABLED,
    SYSTEM_ROLE,
    TOOL_ROLE,
    PreparedRequest,
    ProviderCallContext,
    ProviderResult,
    RenderedMessageView,
    StreamItem,
    assert_modality_capabilities,
    assert_no_disabled_features,
    assert_operator_channel,
    decode_json,
    iter_sse,
)
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.content import to_anthropic_blocks
from intelligence.model_gateway.credentials import SecretValue
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.events import EventFactory, UsageTotals

ANTHROPIC_VERSION = "2023-06-01"
MESSAGES_PATH = "/v1/messages"
CACHE_CONTROL_EPHEMERAL: dict[str, str] = {"type": "ephemeral"}

_STOP_REASONS: dict[str, int] = {
    "end_turn": intelligence_pb2.MODEL_STOP_REASON_END_TURN,
    "stop_sequence": intelligence_pb2.MODEL_STOP_REASON_END_TURN,
    "pause_turn": intelligence_pb2.MODEL_STOP_REASON_END_TURN,
    "tool_use": intelligence_pb2.MODEL_STOP_REASON_TOOL_USE,
    "max_tokens": intelligence_pb2.MODEL_STOP_REASON_MAX_TOKENS,
    "model_context_window_exceeded": intelligence_pb2.MODEL_STOP_REASON_MAX_TOKENS,
    "refusal": intelligence_pb2.MODEL_STOP_REASON_REFUSAL,
}

_ERROR_CODES: dict[str, tuple[str, bool]] = {
    "invalid_request_error": (str(GatewayErrorCode.VALIDATION_SCHEMA), False),
    "authentication_error": (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), False),
    "permission_error": (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), False),
    "not_found_error": (str(GatewayErrorCode.VALIDATION_SCHEMA), False),
    "rate_limit_error": (str(GatewayErrorCode.PROVIDER_RATE_LIMITED), True),
    "overloaded_error": (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), True),
    "api_error": (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), True),
    "timeout_error": (str(GatewayErrorCode.TOOL_TIMEOUT), True),
}


class AnthropicAdapter:
    """Normalizes the Anthropic Messages API to the shared `ModelEvent` contract."""

    kind = ProviderKind.ANTHROPIC

    __slots__ = ()

    def build_request(self, context: ProviderCallContext) -> PreparedRequest:
        call = context.call
        if call.max_output_tokens <= 0:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA, "max_output_tokens must be set for a model call"
            )
        if call.output_constraints_json.strip():
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                "the Anthropic adapter has no structured-output constraint channel; render the "
                "constraint as a forced strict tool instead",
            )
        model = context.route.model
        assert_modality_capabilities(context.messages, model.capabilities)
        cache_enabled = model.supports(capabilities.PROMPT_CACHING)
        system_blocks, messages = _render_messages(context.messages, cache_enabled=cache_enabled)
        body: dict[str, object] = {
            "model": model.model_id,
            "max_tokens": call.max_output_tokens,
            "stream": True,
            "messages": messages,
        }
        if system_blocks:
            body["system"] = system_blocks
        if context.tools:
            tools = [
                {
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.strict_input_schema(),
                }
                for tool in context.tools
            ]
            if cache_enabled:
                tools[-1]["cache_control"] = CACHE_CONTROL_EPHEMERAL
            body["tools"] = tools
        payload = json.dumps(body, sort_keys=True, separators=(",", ":")).encode("utf-8")
        headers: dict[str, str | SecretValue] = {
            "content-type": "application/json",
            "accept": "text/event-stream",
            "anthropic-version": ANTHROPIC_VERSION,
            "x-api-key": context.credential,
            "user-agent": "quansio-intelligence-model-gateway",
        }
        prepared = PreparedRequest(
            provider=context.route.provider.name,
            provider_kind=self.kind,
            model_catalog_id=model.id,
            url=f"{context.base_url}{MESSAGES_PATH}",
            headers=headers,
            body=payload,
            timeout_seconds=_timeout_seconds(context),
            disabled_features=PROVIDER_FEATURES_DISABLED,
        )
        assert_no_disabled_features(prepared.body, prepared.headers)
        assert_operator_channel(context.messages, prepared.body_json())
        return prepared

    def decode(self, factory: EventFactory, lines: Iterator[bytes]) -> Iterator[StreamItem]:
        usage = UsageTotals()
        stop_reason: int | None = None
        block_kinds: dict[int, str] = {}
        tool_state: dict[int, tuple[str, str, list[str]]] = {}
        thinking_state: dict[int, list[str]] = {}
        for event_name, data in iter_sse(lines):
            if not data or data == "[DONE]":
                continue
            payload = decode_json(data)
            if event_name == "error" or payload.get("type") == "error":
                yield _error_result(payload)
                return
            event_type = payload.get("type")
            if event_type == "message_start":
                usage = factory.observe_usage(_usage_from(payload.get("message")))
            elif event_type == "content_block_start":
                index = _int(payload.get("index"))
                block = payload.get("content_block")
                block = block if isinstance(block, dict) else {}
                block_type = str(block.get("type", ""))
                block_kinds[index] = block_type
                if block_type == "tool_use":
                    tool_state[index] = (
                        str(block.get("id", "")),
                        str(block.get("name", "")),
                        [],
                    )
                elif block_type == "thinking":
                    thinking_state[index] = []
            elif event_type == "content_block_delta":
                index = _int(payload.get("index"))
                delta = payload.get("delta")
                delta = delta if isinstance(delta, dict) else {}
                delta_type = delta.get("type")
                if delta_type == "text_delta":
                    yield factory.delta(str(delta.get("text", "")))
                elif delta_type == "input_json_delta":
                    state = tool_state.get(index)
                    if state is not None:
                        state[2].append(str(delta.get("partial_json", "")))
                elif delta_type == "thinking_delta":
                    thinking_state.setdefault(index, []).append(str(delta.get("thinking", "")))
            elif event_type == "content_block_stop":
                index = _int(payload.get("index"))
                if block_kinds.get(index) == "tool_use":
                    tool_call_id, name, chunks = tool_state.pop(index, ("", "", []))
                    yield factory.tool_call(tool_call_id, name, _tool_args(chunks))
                elif block_kinds.get(index) == "thinking":
                    summary = "".join(thinking_state.pop(index, []))
                    if summary:
                        yield factory.thinking_summary(summary)
            elif event_type == "message_delta":
                usage = factory.observe_usage(_usage_from(payload))
                delta = payload.get("delta")
                if isinstance(delta, dict) and isinstance(delta.get("stop_reason"), str):
                    stop_reason = _STOP_REASONS.get(str(delta["stop_reason"]))
            elif event_type == "message_stop":
                break
        if stop_reason is None:
            yield ProviderResult(
                stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
                usage=usage,
                error_code=str(GatewayErrorCode.PROVIDER_UNAVAILABLE),
                retryable=True,
            )
            return
        yield ProviderResult(stop_reason=stop_reason, usage=usage)


def _render_messages(
    messages: tuple[RenderedMessageView, ...], *, cache_enabled: bool
) -> tuple[list[dict[str, object]], list[dict[str, object]]]:
    system_blocks: list[dict[str, object]] = []
    conversation: list[dict[str, object]] = []
    last_hinted = -1
    if cache_enabled:
        for index, message in enumerate(messages):
            if message.cache_hint:
                last_hinted = index
    for index, message in enumerate(messages):
        hinted = cache_enabled and index == last_hinted
        if message.role in (SYSTEM_ROLE, OPERATOR_ROLE):
            blocks = to_anthropic_blocks(message.content)
            if hinted:
                _mark_cache(blocks)
            system_blocks.extend(blocks)
            continue
        if message.role == TOOL_ROLE:
            tool_blocks: list[dict[str, object]] = [
                {
                    "type": "tool_result",
                    "tool_use_id": message.content.tool_call_id,
                    "content": to_anthropic_blocks(message.content) or [{"type": "text", "text": ""}],
                }
            ]
            _append_turn(conversation, "user", tool_blocks)
            continue
        role = "assistant" if message.role == ASSISTANT_ROLE else "user"
        blocks = to_anthropic_blocks(message.content)
        if hinted:
            _mark_cache(blocks)
        _append_turn(conversation, role, blocks)
    return system_blocks, conversation


def _append_turn(conversation: list[dict[str, object]], role: str, blocks: list[dict[str, object]]) -> None:
    """The Messages API requires alternating roles; consecutive same-role turns are merged."""
    if conversation and conversation[-1].get("role") == role:
        existing = conversation[-1].get("content")
        if isinstance(existing, list):
            existing.extend(blocks)
            return
    conversation.append({"role": role, "content": blocks})


def _mark_cache(blocks: list[dict[str, object]]) -> None:
    if blocks:
        blocks[-1]["cache_control"] = CACHE_CONTROL_EPHEMERAL


def _tool_args(chunks: list[str]) -> str:
    joined = "".join(chunks).strip() or "{}"
    try:
        decoded: Any = json.loads(joined)
    except json.JSONDecodeError as error:
        raise GatewayError(
            GatewayErrorCode.PROVIDER_UNAVAILABLE,
            "provider streamed tool arguments that are not valid JSON",
        ) from error
    if not isinstance(decoded, dict):
        raise GatewayError(
            GatewayErrorCode.PROVIDER_UNAVAILABLE,
            "provider streamed tool arguments that are not a JSON object",
        )
    return json.dumps(decoded, sort_keys=True, separators=(",", ":"))


def _usage_from(payload: object) -> UsageTotals:
    if not isinstance(payload, dict):
        return UsageTotals()
    usage = payload.get("usage")
    if not isinstance(usage, dict):
        return UsageTotals()
    return UsageTotals(
        input_tokens=_int(usage.get("input_tokens")),
        output_tokens=_int(usage.get("output_tokens")),
        cache_read_tokens=_int(usage.get("cache_read_input_tokens")),
        cache_write_tokens=_int(usage.get("cache_creation_input_tokens")),
    )


def _error_result(payload: dict[str, object]) -> ProviderResult:
    error = payload.get("error")
    error = error if isinstance(error, dict) else {}
    error_type = str(error.get("type", ""))
    code, retryable = _ERROR_CODES.get(error_type, (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), True))
    return ProviderResult(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
        usage=UsageTotals(),
        error_code=code,
        retryable=retryable,
    )


def _timeout_seconds(context: ProviderCallContext) -> float:
    return max(context.call.timeout_ms / 1000.0, 0.001)


def _int(value: object) -> int:
    return value if isinstance(value, int) and value > 0 else 0
