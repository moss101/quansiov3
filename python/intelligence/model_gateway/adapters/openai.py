"""OpenAI chat-completions adapter (second provider, OD-004).

Wire reference: `POST {base_url}/v1/chat/completions` with `stream: true` and
`stream_options.include_usage`. The model id comes from the catalog.

Channel mapping (DOMAIN.md §11.1):

* `ROLE_SYSTEM` → `system`, `ROLE_OPERATOR` → `developer` (the operator channel for the
  current model family), never a fake user turn.
* `ROLE_TOOL` → a `tool` message carrying `tool_call_id`.
* Tools are sent as strict functions (`strict: true` plus an `additionalProperties: false`
  object schema), and `parallel_tool_calls` is disabled so one tool call maps to one runtime
  ToolCall.
* OpenAI performs prompt caching automatically and exposes no request-side cache flag, so the
  cache hint only orders content (stable prefix first); no cache parameter is invented.
* `max_completion_tokens` is the current chat-completions field; the OpenAI-compatible adapter
  overrides it for older self-hosted servers.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from typing import Any

from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway.adapters.base import (
    ASSISTANT_ROLE,
    OPERATOR_ROLE,
    PROVIDER_FEATURES_DISABLED,
    SYSTEM_ROLE,
    TOOL_ROLE,
    USER_ROLE,
    PreparedRequest,
    ProviderCallContext,
    ProviderResult,
    StreamItem,
    assert_modality_capabilities,
    assert_no_disabled_features,
    assert_operator_channel,
    decode_json,
    iter_sse,
)
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.content import to_openai_blocks
from intelligence.model_gateway.credentials import SecretValue
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.events import EventFactory, UsageTotals

CHAT_COMPLETIONS_PATH = "/v1/chat/completions"

_FINISH_REASONS: dict[str, int] = {
    "stop": intelligence_pb2.MODEL_STOP_REASON_END_TURN,
    "tool_calls": intelligence_pb2.MODEL_STOP_REASON_TOOL_USE,
    "function_call": intelligence_pb2.MODEL_STOP_REASON_TOOL_USE,
    "length": intelligence_pb2.MODEL_STOP_REASON_MAX_TOKENS,
    "content_filter": intelligence_pb2.MODEL_STOP_REASON_REFUSAL,
}

_ERROR_CODES: dict[str, tuple[str, bool]] = {
    "invalid_request_error": (str(GatewayErrorCode.VALIDATION_SCHEMA), False),
    "authentication_error": (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), False),
    "permission_error": (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), False),
    "not_found_error": (str(GatewayErrorCode.VALIDATION_SCHEMA), False),
    "rate_limit_exceeded": (str(GatewayErrorCode.PROVIDER_RATE_LIMITED), True),
    "server_error": (str(GatewayErrorCode.PROVIDER_UNAVAILABLE), True),
}


class OpenAIAdapter:
    """Normalizes OpenAI chat completions to the shared `ModelEvent` contract."""

    kind = ProviderKind.OPENAI

    # Family switches; the OpenAI-compatible adapter narrows them for self-hosted servers.
    MAX_TOKENS_FIELD = "max_completion_tokens"
    SEND_STREAM_OPTIONS = True
    SEND_PARALLEL_TOOL_CALLS = True

    __slots__ = ()

    def build_request(self, context: ProviderCallContext) -> PreparedRequest:
        call = context.call
        if call.max_output_tokens <= 0:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA, "max_output_tokens must be set for a model call"
            )
        model = context.route.model
        assert_modality_capabilities(context.messages, model.capabilities)
        body: dict[str, object] = {
            "model": model.model_id,
            "stream": True,
            "messages": self._render_messages(context),
            self.MAX_TOKENS_FIELD: call.max_output_tokens,
        }
        if self.SEND_STREAM_OPTIONS:
            body["stream_options"] = {"include_usage": True}
        if self.SEND_PARALLEL_TOOL_CALLS:
            body["parallel_tool_calls"] = False
        if context.tools:
            body["tools"] = [
                {
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.strict_input_schema(),
                        "strict": True,
                    },
                }
                for tool in context.tools
            ]
        constraints = call.output_constraints_json.strip()
        if constraints:
            try:
                decoded: Any = json.loads(constraints)
            except json.JSONDecodeError as error:
                raise GatewayError(
                    GatewayErrorCode.VALIDATION_SCHEMA, "output_constraints_json is not valid JSON"
                ) from error
            if not isinstance(decoded, dict):
                raise GatewayError(
                    GatewayErrorCode.VALIDATION_SCHEMA,
                    "output_constraints_json must be a JSON object",
                )
            body["response_format"] = decoded
        payload = json.dumps(body, sort_keys=True, separators=(",", ":")).encode("utf-8")
        headers: dict[str, str | SecretValue] = {
            "content-type": "application/json",
            "accept": "text/event-stream",
            "authorization": context.credential.with_scheme("Bearer"),
            "user-agent": "quansio-intelligence-model-gateway",
        }
        prepared = PreparedRequest(
            provider=context.route.provider.name,
            provider_kind=self.kind,
            model_catalog_id=model.id,
            url=f"{context.base_url}{CHAT_COMPLETIONS_PATH}",
            headers=headers,
            body=payload,
            timeout_seconds=max(call.timeout_ms / 1000.0, 0.001),
            disabled_features=PROVIDER_FEATURES_DISABLED,
        )
        assert_no_disabled_features(prepared.body, prepared.headers)
        assert_operator_channel(context.messages, prepared.body_json())
        return prepared

    def _render_messages(self, context: ProviderCallContext) -> list[dict[str, object]]:
        messages: list[dict[str, object]] = []
        for message in context.messages:
            if message.role == SYSTEM_ROLE:
                messages.append({"role": "system", "content": message.content.text()})
            elif message.role == OPERATOR_ROLE:
                messages.append({"role": "developer", "content": message.content.text()})
            elif message.role == TOOL_ROLE:
                messages.append(
                    {
                        "role": "tool",
                        "tool_call_id": message.content.tool_call_id,
                        "content": message.content.text(),
                    }
                )
            elif message.role == ASSISTANT_ROLE:
                messages.append(
                    {"role": "assistant", "content": to_openai_blocks(message.content, allow_documents=False)}
                )
            elif message.role == USER_ROLE:
                messages.append(
                    {"role": "user", "content": to_openai_blocks(message.content, allow_documents=False)}
                )
        return messages

    def decode(self, factory: EventFactory, lines: Iterator[bytes]) -> Iterator[StreamItem]:
        usage = UsageTotals()
        stop_reason: int | None = None
        refusal = False
        tool_state: dict[int, list[str]] = {}
        tool_ids: dict[int, tuple[str, str]] = {}
        for _event_name, data in iter_sse(lines):
            if not data or data == "[DONE]":
                continue
            payload = decode_json(data)
            if isinstance(payload.get("error"), dict):
                yield _error_result(payload)
                return
            combined = usage.combine(_usage_from(payload))
            usage = factory.observe_usage(combined)
            choices = payload.get("choices")
            if not isinstance(choices, list):
                continue
            for choice in choices:
                if not isinstance(choice, dict):
                    continue
                delta = choice.get("delta")
                if isinstance(delta, dict):
                    reasoning = delta.get("reasoning_content") or delta.get("reasoning")
                    if isinstance(reasoning, str) and reasoning:
                        yield factory.thinking_summary(reasoning)
                    if isinstance(delta.get("refusal"), str):
                        refusal = True
                        if delta["refusal"]:
                            yield factory.delta(str(delta["refusal"]))
                    if isinstance(delta.get("content"), str) and delta["content"]:
                        yield factory.delta(str(delta["content"]))
                    tool_calls = delta.get("tool_calls")
                    if isinstance(tool_calls, list):
                        _accumulate_tool_calls(tool_calls, tool_ids, tool_state)
                finish = choice.get("finish_reason")
                if isinstance(finish, str):
                    if finish == "content_filter":
                        refusal = True
                    stop_reason = _FINISH_REASONS.get(finish)
        for index in sorted(tool_state):
            tool_call_id, name = tool_ids.get(index, ("", ""))
            yield factory.tool_call(tool_call_id, name, _tool_args(tool_state[index]))
        if refusal:
            yield ProviderResult(stop_reason=intelligence_pb2.MODEL_STOP_REASON_REFUSAL, usage=usage)
            return
        if stop_reason is None:
            yield ProviderResult(
                stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
                usage=usage,
                error_code=str(GatewayErrorCode.PROVIDER_UNAVAILABLE),
                retryable=True,
            )
            return
        yield ProviderResult(stop_reason=stop_reason, usage=usage)


def _accumulate_tool_calls(
    tool_calls: list[object],
    tool_ids: dict[int, tuple[str, str]],
    tool_state: dict[int, list[str]],
) -> None:
    for entry in tool_calls:
        if not isinstance(entry, dict):
            continue
        index = entry.get("index")
        index = index if isinstance(index, int) else 0
        function = entry.get("function")
        function = function if isinstance(function, dict) else {}
        tool_id = entry.get("id")
        name = function.get("name")
        if tool_id is not None or name is not None:
            previous_id, previous_name = tool_ids.get(index, ("", ""))
            tool_ids[index] = (
                str(tool_id) if tool_id is not None else previous_id,
                str(name) if name is not None else previous_name,
            )
        arguments = function.get("arguments")
        if isinstance(arguments, str):
            tool_state.setdefault(index, []).append(arguments)


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


def _usage_from(payload: dict[str, object]) -> UsageTotals:
    usage = payload.get("usage")
    if not isinstance(usage, dict):
        return UsageTotals()
    details = usage.get("prompt_tokens_details")
    details = details if isinstance(details, dict) else {}
    return UsageTotals(
        input_tokens=_int(usage.get("prompt_tokens")),
        output_tokens=_int(usage.get("completion_tokens")),
        cache_read_tokens=_int(details.get("cached_tokens")),
        cache_write_tokens=0,
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


def _int(value: object) -> int:
    return value if isinstance(value, int) and value > 0 else 0
