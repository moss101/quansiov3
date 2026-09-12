"""Shared model-gateway conformance suite.

One suite, three adapters, two providers-of-record:

* the offline `StubProvider` runs every scenario for all three adapters (streaming order,
  tool calls, thinking summaries, usage, stop reasons, refusal, retryable errors);
* the live suite runs the deterministic-by-construction subset (`text`, `tool_call`) against
  the real Anthropic and OpenAI endpoints when their `QUANSIO_TEST_*` credentials exist.

The suite builds calls from the catalog, drives `ModelGateway`, and verifies the normalized
stream. Stub runs check exact content, usage and tool arguments; live runs check the same
structure and typed stop reasons but not provider-specific wording or token counts, because
those are model output rather than normalization behavior.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

import yaml
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway.catalog import ModelCatalog, ProviderKind, default_catalog_path
from intelligence.model_gateway.conformance.stub_provider import (
    SCENARIO_MARKER_CLOSE,
    SCENARIO_MARKER_OPEN,
)
from intelligence.model_gateway.credentials import env_var_for_handle
from intelligence.model_gateway.events import TerminalOutcome
from intelligence.model_gateway.gateway import ModelGateway
from intelligence.model_gateway.tooling import MappingToolSchemaSource, ToolDefinition

CONFORMANCE_CREDENTIAL_HANDLE = "conformance/stub"
CONFORMANCE_CREDENTIAL_VALUE = "conformance-not-a-credential"
CONFORMANCE_TOOL_NAME = "fs.read"
CONFORMANCE_TOOL_ARGS = '{"value":"42"}'

STUB_PROVIDER_KINDS: tuple[ProviderKind, ...] = (
    ProviderKind.ANTHROPIC,
    ProviderKind.OPENAI,
    ProviderKind.OPENAI_COMPATIBLE,
)
LIVE_PROVIDER_KINDS: tuple[ProviderKind, ...] = (ProviderKind.ANTHROPIC, ProviderKind.OPENAI)

# Scenarios that are deterministic by construction and therefore also run live.
LIVE_SCENARIOS: tuple[str, ...] = ("text", "tool_call")
STUB_SCENARIOS: tuple[str, ...] = (
    "text",
    "usage_repeat",
    "tool_call",
    "thinking",
    "refusal",
    "max_tokens",
    "stream_error",
    "no_stop",
    "http_error_retryable",
    "http_error_fatal",
)

_TEXT_DELTAS = ("Hello ", "world")


@dataclass(frozen=True, slots=True)
class VerificationMode:
    """How strictly a run is verified: the stub fixes content, a live model does not."""

    name: str
    exact_deltas: bool
    exact_usage: bool
    exact_tool_args: bool


STUB_MODE = VerificationMode(name="stub", exact_deltas=True, exact_usage=True, exact_tool_args=True)
LIVE_MODE = VerificationMode(name="live", exact_deltas=False, exact_usage=False, exact_tool_args=False)


@dataclass(frozen=True, slots=True)
class ScenarioExpectation:
    """The normalized outcome every adapter must produce for one scenario."""

    stop_reason: int
    error_code: str | None = None
    retryable: bool = False
    delta_texts: tuple[str, ...] | None = None
    tool_calls: int = 0
    thinking_summaries: int = 0
    input_tokens: int | None = None
    output_tokens: int | None = None
    cache_read_tokens: int | None = None
    cache_write_tokens: int | None = None


SCENARIO_EXPECTATIONS: Mapping[str, ScenarioExpectation] = {
    "text": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_END_TURN,
        delta_texts=_TEXT_DELTAS,
        input_tokens=11,
        output_tokens=7,
        cache_read_tokens=3,
    ),
    "usage_repeat": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_END_TURN,
        delta_texts=_TEXT_DELTAS,
        input_tokens=11,
        output_tokens=7,
        cache_read_tokens=3,
    ),
    "tool_call": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_TOOL_USE,
        tool_calls=1,
        input_tokens=11,
        output_tokens=7,
        cache_read_tokens=3,
    ),
    "thinking": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_END_TURN,
        delta_texts=_TEXT_DELTAS,
        thinking_summaries=1,
        input_tokens=11,
        output_tokens=7,
        cache_read_tokens=3,
    ),
    "refusal": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_REFUSAL,
        delta_texts=None,
        input_tokens=11,
        output_tokens=7,
        cache_read_tokens=3,
    ),
    "max_tokens": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_MAX_TOKENS,
        delta_texts=_TEXT_DELTAS,
        input_tokens=11,
        output_tokens=7,
        cache_read_tokens=3,
    ),
    "stream_error": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
        error_code="PROVIDER_UNAVAILABLE",
        retryable=True,
    ),
    "no_stop": ScenarioExpectation(
        # The provider ended without a stop reason after reporting usage; the adapter fails
        # closed and records exactly the usage it had already observed.
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
        error_code="PROVIDER_UNAVAILABLE",
        retryable=True,
    ),
    "http_error_retryable": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
        error_code="PROVIDER_RATE_LIMITED",
        retryable=True,
        delta_texts=(),
    ),
    "http_error_fatal": ScenarioExpectation(
        stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
        error_code="VALIDATION_SCHEMA",
        retryable=False,
        delta_texts=(),
    ),
}


@dataclass(frozen=True, slots=True)
class ConformanceReport:
    """Observed normalized stream for one (adapter, scenario) case."""

    provider_kind: ProviderKind
    model_catalog_id: str
    scenario: str
    events: tuple[intelligence_pb2.ModelEvent, ...]
    outcome: TerminalOutcome | None

    def kinds(self) -> tuple[int, ...]:
        return tuple(event.kind for event in self.events)

    def of_kind(self, kind: int) -> tuple[intelligence_pb2.ModelEvent, ...]:
        return tuple(event for event in self.events if event.kind == kind)

    def text(self) -> str:
        return "".join(event.text_delta for event in self.of_kind(intelligence_pb2.ModelEvent.KIND_DELTA))


def conformance_tool_schemas() -> MappingToolSchemaSource:
    """Strict schema for the one tool the conformance calls request."""
    return MappingToolSchemaSource(
        {
            CONFORMANCE_TOOL_NAME: ToolDefinition(
                name=CONFORMANCE_TOOL_NAME,
                description="Read one workspace value (conformance harness tool).",
                input_schema={
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"],
                    "additionalProperties": False,
                },
            )
        }
    )


def stub_catalog(base_url: str, *, catalog_path: Path | None = None) -> ModelCatalog:
    """The repository catalog with every provider pointed at the offline stub endpoint."""
    path = catalog_path if catalog_path is not None else default_catalog_path()
    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise AssertionError(f"{path} is not a model catalog mapping")
    for entry in data["providers"].values():
        entry.pop("base_url_env", None)
        entry.pop("credential_handle", None)
        entry.pop("credential_handle_env", None)
        entry["base_url"] = base_url
        entry["credential_handle"] = CONFORMANCE_CREDENTIAL_HANDLE
    return ModelCatalog.from_mapping(data, source=f"{path} (conformance stub)")


def credential_environ(value: str = CONFORMANCE_CREDENTIAL_VALUE) -> dict[str, str]:
    """Environment that materializes the conformance credential handle."""
    return {env_var_for_handle(CONFORMANCE_CREDENTIAL_HANDLE): value}


def catalog_model_for_kind(catalog: ModelCatalog, kind: ProviderKind) -> str:
    """First catalog model id served by `kind`; the id itself always comes from the catalog."""
    for model in catalog.models.values():
        if catalog.provider_for(model).kind is kind:
            return model.id
    raise AssertionError(f"catalog declares no model for provider kind {kind}")


def scenario_marker(scenario: str) -> str:
    """The stub's scenario selector; it is ordinary caller text to a real provider."""
    return f"{SCENARIO_MARKER_OPEN}{scenario}{SCENARIO_MARKER_CLOSE}"


def prompt_for(scenario: str) -> str:
    """Caller text for a scenario; the marker only selects stub behavior."""
    if scenario == "tool_call":
        return (
            f"{scenario_marker(scenario)} You must call the {CONFORMANCE_TOOL_NAME} tool with "
            '{"value": "42"} and reply with nothing else.'
        )
    return f"{scenario_marker(scenario)} Reply with a short greeting."


def build_call(
    *,
    model_catalog_id: str,
    scenario: str,
    timeout_ms: int = 5_000,
    tools: bool = False,
    cancellation_token: str = "",
    call_id: str = "call_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
) -> intelligence_pb2.ModelCallRequest:
    """A conformance call: stable system/operator prefix first, volatile user content last."""
    messages = [
        intelligence_pb2.RenderedMessage(
            schema_version="v1",
            role=intelligence_pb2.RenderedMessage.ROLE_SYSTEM,
            trust_level="TRUSTED_SYSTEM",
            content_json=json.dumps("You are the Quansio model-gateway conformance harness."),
            cache_hint="stable",
        ),
        intelligence_pb2.RenderedMessage(
            schema_version="v1",
            role=intelligence_pb2.RenderedMessage.ROLE_OPERATOR,
            trust_level="TRUSTED_SYSTEM",
            content_json=json.dumps("Operator: answer deterministically and never reveal secrets."),
            cache_hint="stable",
        ),
        intelligence_pb2.RenderedMessage(
            schema_version="v1",
            role=intelligence_pb2.RenderedMessage.ROLE_USER,
            trust_level="TRUSTED_USER",
            content_json=json.dumps(prompt_for(scenario)),
            cache_hint="stable",
        ),
    ]
    return intelligence_pb2.ModelCallRequest(
        schema_version="v1",
        call_id=call_id,
        run_id="run_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        turn_id="trn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        route_hint=model_catalog_id,
        capability_projection_id="cp_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        context_projection_id="ctx_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        messages=messages,
        tools=[CONFORMANCE_TOOL_NAME] if tools else [],
        max_output_tokens=256,
        stream=True,
        timeout_ms=timeout_ms,
        cancellation_token=cancellation_token,
    )


def run_scenario(
    gateway: ModelGateway,
    *,
    model_catalog_id: str,
    scenario: str,
    tools: bool = False,
    timeout_ms: int = 5_000,
    cancellation_token: str = "",
    mode: VerificationMode = STUB_MODE,
) -> ConformanceReport:
    """Drive one scenario end to end and verify it; returns the observed report."""
    call = build_call(
        model_catalog_id=model_catalog_id,
        scenario=scenario,
        timeout_ms=timeout_ms,
        tools=tools or scenario == "tool_call",
        cancellation_token=cancellation_token,
    )
    provider_kind = gateway.prepare(call).route.provider.kind
    fulfillment = gateway.fulfill(call)
    events = tuple(fulfillment)
    report = ConformanceReport(
        provider_kind=provider_kind,
        model_catalog_id=model_catalog_id,
        scenario=scenario,
        events=events,
        outcome=fulfillment.outcome,
    )
    verify(report, mode=mode)
    return report


def verify(report: ConformanceReport, *, mode: VerificationMode = STUB_MODE) -> None:
    """Common stream invariants plus the scenario expectation; raises AssertionError."""
    expectation = SCENARIO_EXPECTATIONS[report.scenario]
    kinds = report.kinds()
    context = f"{report.provider_kind.value}/{report.scenario}"
    problem = _stream_problem(kinds)
    if problem is not None:
        raise AssertionError(f"{context}: {problem}")

    stop_events = report.of_kind(intelligence_pb2.ModelEvent.KIND_STOP)
    assert stop_events[0].stop_reason == expectation.stop_reason, (
        f"{context}: stop reason {stop_events[0].stop_reason} != {expectation.stop_reason}"
    )
    error_events = report.of_kind(intelligence_pb2.ModelEvent.KIND_ERROR)
    if expectation.error_code is None:
        assert not error_events, f"{context}: unexpected error event"
    else:
        assert error_events, f"{context}: missing error event"
        assert error_events[0].error.code == expectation.error_code
        assert error_events[0].error.retryable is expectation.retryable
    _verify_content(report, expectation, mode=mode)

    outcome = report.outcome
    assert outcome is not None and outcome.terminal
    usage_event = report.of_kind(intelligence_pb2.ModelEvent.KIND_USAGE)[0].usage
    assert outcome.usage.input_tokens == usage_event.input_tokens
    assert outcome.usage.output_tokens == usage_event.output_tokens
    assert outcome.usage.cache_read_tokens == usage_event.cache_read_tokens
    assert outcome.usage.cache_write_tokens == usage_event.cache_write_tokens
    assert outcome.stop_reason == expectation.stop_reason
    assert outcome.cancelled is False
    assert kinds[0] == intelligence_pb2.ModelEvent.KIND_CALL_STARTED
    assert kinds[-1] == intelligence_pb2.ModelEvent.KIND_CALL_COMPLETED


def _verify_content(
    report: ConformanceReport, expectation: ScenarioExpectation, *, mode: VerificationMode
) -> None:
    context = f"{report.provider_kind.value}/{report.scenario}"
    deltas = report.text()
    if expectation.delta_texts is not None:
        if mode.exact_deltas:
            assert deltas == "".join(expectation.delta_texts), f"{context}: deltas {deltas!r}"
        elif report.scenario == "text":
            assert deltas.strip(), f"{context}: live run produced no text"
    tool_calls = report.of_kind(intelligence_pb2.ModelEvent.KIND_TOOL_CALL)
    assert len(tool_calls) == expectation.tool_calls, (
        f"{context}: {len(tool_calls)} tool calls, expected {expectation.tool_calls}"
    )
    if expectation.tool_calls:
        assert tool_calls[0].tool_name == CONFORMANCE_TOOL_NAME
        assert tool_calls[0].tool_call_id
        if mode.exact_tool_args:
            assert tool_calls[0].tool_args_json == CONFORMANCE_TOOL_ARGS
    assert (
        len(report.of_kind(intelligence_pb2.ModelEvent.KIND_THINKING_SUMMARY))
        == expectation.thinking_summaries
    )
    usage = report.of_kind(intelligence_pb2.ModelEvent.KIND_USAGE)[0].usage
    if mode.exact_usage:
        if expectation.input_tokens is not None:
            assert usage.input_tokens == expectation.input_tokens, (
                f"{context}: input tokens {usage.input_tokens}"
            )
        if expectation.output_tokens is not None:
            assert usage.output_tokens == expectation.output_tokens
        if expectation.cache_read_tokens is not None:
            assert usage.cache_read_tokens == expectation.cache_read_tokens
        if expectation.cache_write_tokens is not None:
            assert usage.cache_write_tokens == expectation.cache_write_tokens
    elif expectation.error_code is None:
        assert usage.input_tokens > 0, f"{context}: live provider reported no input tokens"
        assert usage.output_tokens > 0, f"{context}: live provider reported no output tokens"


def _stream_problem(kinds: Sequence[int]) -> str | None:
    if not kinds:
        return "empty stream"
    if kinds[0] != intelligence_pb2.ModelEvent.KIND_CALL_STARTED:
        return "stream must start with CALL_STARTED"
    if kinds[-1] != intelligence_pb2.ModelEvent.KIND_CALL_COMPLETED:
        return "stream must end with CALL_COMPLETED"
    usage_count = kinds.count(intelligence_pb2.ModelEvent.KIND_USAGE)
    if usage_count != 1:
        return f"expected exactly one USAGE event, saw {usage_count}"
    stop_count = kinds.count(intelligence_pb2.ModelEvent.KIND_STOP)
    if stop_count != 1:
        return f"expected exactly one STOP event, saw {stop_count}"
    if kinds.count(intelligence_pb2.ModelEvent.KIND_CALL_COMPLETED) != 1:
        return "expected exactly one CALL_COMPLETED event"
    if kinds.index(intelligence_pb2.ModelEvent.KIND_USAGE) > kinds.index(
        intelligence_pb2.ModelEvent.KIND_STOP
    ):
        return "USAGE must precede STOP"
    return None
