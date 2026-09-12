"""Credential custody and the INT-002 secret-leak scan.

The gateway is the only component that reads a provider credential and the only one that puts
it on the wire. These tests plant a canary credential (and a canary prompt) and scan everything
the gateway produces — request objects, wire bytes, logs, events and error messages — for the
canary. The credential is allowed in exactly one place: the authentication header of the
outgoing provider request. Everything else must fail the scan.

The runtime never logs prompt content, so the prompt canary must appear only inside the request
body bytes that are transmitted to the provider.
"""

from __future__ import annotations

import json
import logging
from collections.abc import Iterator
from typing import Any

import pytest
from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.catalog import ProviderConfig, ProviderKind
from intelligence.model_gateway.conformance import (
    StubProvider,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    stub_catalog,
)
from intelligence.model_gateway.credentials import CredentialResolver, SecretValue, env_var_for_handle
from intelligence.model_gateway.errors import GatewayError

CANARY_CREDENTIAL = "sk-canary-int002-never-log-4f9c1a"
CANARY_PROMPT = "prompt-canary-int002-never-log"
LOGGER_NAME = "intelligence.model_gateway"
LOGGED_KEYS = {
    "event",
    "call_id",
    "route_id",
    "stop_reason",
    "error_code",
    "retryable",
    "cancelled",
    "latency_ms",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
}
AUTH_HEADERS = {"x-api-key", "authorization"}


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


@pytest.fixture(scope="module")
def catalog(stub: StubProvider) -> Any:
    return stub_catalog(stub.base_url)


@pytest.fixture(scope="module")
def gateway(catalog: Any) -> ModelGateway:
    return ModelGateway(
        catalog=catalog,
        environ=credential_environ(CANARY_CREDENTIAL),
        tool_schemas=conformance_tool_schemas(),
    )


def test_secret_value_redacts_itself() -> None:
    secret = SecretValue("provider/anthropic", CANARY_CREDENTIAL)
    assert CANARY_CREDENTIAL not in repr(secret)
    assert CANARY_CREDENTIAL not in str(secret)
    assert CANARY_CREDENTIAL not in f"{secret}"
    assert CANARY_CREDENTIAL not in f"{secret.with_scheme('Bearer')}"
    assert secret.reveal() == CANARY_CREDENTIAL
    assert secret.with_scheme("Bearer").reveal() == f"Bearer {CANARY_CREDENTIAL}"


def test_credential_handle_indirection_reads_only_secret_custody() -> None:
    """`credential_handle_env` names a handle; the handle names the materialization variable."""
    environ = {
        "QUANSIO_OPENAI_COMPATIBLE_KEY_HANDLE": "provider/compatible",
        env_var_for_handle("provider/compatible"): CANARY_CREDENTIAL,
    }
    provider = ProviderConfig(
        name="openai-compatible",
        kind=ProviderKind.OPENAI_COMPATIBLE,
        base_url=None,
        base_url_env="QUANSIO_OPENAI_COMPATIBLE_BASE_URL",
        credential_handle=None,
        credential_handle_env="QUANSIO_OPENAI_COMPATIBLE_KEY_HANDLE",
    )
    secret = CredentialResolver(environ).resolve(provider)
    assert secret.handle == "provider/compatible"
    assert secret.reveal() == CANARY_CREDENTIAL
    assert CANARY_CREDENTIAL not in repr(secret)
    with pytest.raises(GatewayError):
        CredentialResolver({}).resolve(provider)


@pytest.mark.parametrize("kind_value", ["anthropic", "openai", "openai_compatible"])
def test_credential_reaches_only_the_authentication_header(
    gateway: ModelGateway, stub: StubProvider, kind_value: str
) -> None:
    kind = ProviderKind(kind_value)
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    call = build_call(model_catalog_id=model_id, scenario="text")
    call.messages[-1].content_json = json.dumps(CANARY_PROMPT)

    prepared = gateway.prepare(call).prepared_request
    assert CANARY_CREDENTIAL not in repr(prepared)
    assert CANARY_CREDENTIAL not in repr(prepared.as_http_request())
    assert CANARY_PROMPT not in repr(prepared), "prompt content must not appear in request reprs"

    before = len(stub.state.calls)
    events = list(gateway.fulfill(call))
    recorded = stub.state.calls[before]

    leaked_headers = [
        name
        for name, value in recorded.headers.items()
        if CANARY_CREDENTIAL in value and name not in AUTH_HEADERS
    ]
    assert leaked_headers == [], f"credential leaked into headers {leaked_headers}"
    auth = {name: recorded.headers.get(name, "") for name in AUTH_HEADERS}
    assert any(CANARY_CREDENTIAL in value for value in auth.values()), (
        "the credential must be presented to the provider it authenticates"
    )
    assert CANARY_CREDENTIAL.encode() not in recorded.body, "credential must never be in the body"
    assert CANARY_PROMPT.encode() in recorded.body, "the prompt is the payload, not a secret"
    for event in events:
        assert CANARY_CREDENTIAL not in event.SerializeToString().decode("latin-1")


def test_logs_events_and_errors_never_carry_credentials_or_prompts(
    gateway: ModelGateway, stub: StubProvider, caplog: pytest.LogCaptureFixture
) -> None:
    call = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC), scenario="text"
    )
    call.messages[-1].content_json = json.dumps(CANARY_PROMPT)
    with caplog.at_level(logging.DEBUG, logger=LOGGER_NAME):
        events = list(gateway.fulfill(call))
        failing = build_call(model_catalog_id=call.route_hint, scenario="http_error_fatal")
        failing.messages[-1].content_json = json.dumps(CANARY_PROMPT)
        failing_events = list(gateway.fulfill(failing))

    text = caplog.text
    assert CANARY_CREDENTIAL not in text
    assert CANARY_PROMPT not in text
    for event in [*events, *failing_events]:
        assert CANARY_CREDENTIAL not in event.SerializeToString().decode("latin-1")
        assert CANARY_PROMPT not in event.text_delta
    assert caplog.records, "a completed call must be observable in logs"


def test_gateway_logging_has_a_fixed_secret_free_key_set(
    gateway: ModelGateway, caplog: pytest.LogCaptureFixture
) -> None:
    call = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC), scenario="text"
    )
    with caplog.at_level(logging.INFO, logger=LOGGER_NAME):
        list(gateway.fulfill(call))
    records = [json.loads(record.getMessage()) for record in caplog.records]
    assert len(records) == 1
    assert set(records[0]) == LOGGED_KEYS
    assert records[0]["event"] == "model_gateway.call_completed"
    assert records[0]["call_id"] == call.call_id


def test_missing_credential_fails_closed_without_naming_a_value(stub: StubProvider) -> None:
    gateway = ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ={},
        tool_schemas=conformance_tool_schemas(),
    )
    call = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC),
        scenario="text",
    )
    with pytest.raises(GatewayError) as excinfo:
        gateway.fulfill(call)
    message = str(excinfo.value)
    assert env_var_for_handle("conformance/stub") in message
    assert CANARY_CREDENTIAL not in message
    assert excinfo.value.retryable is False


def test_error_messages_never_echo_provider_bodies(stub: StubProvider) -> None:
    """A provider error body can echo request content; only the typed code is surfaced."""
    gateway = ModelGateway(
        catalog=stub_catalog(stub.base_url),
        environ=credential_environ(CANARY_CREDENTIAL),
        tool_schemas=conformance_tool_schemas(),
    )
    call = build_call(
        model_catalog_id=catalog_model_for_kind(gateway.catalog, ProviderKind.ANTHROPIC),
        scenario="http_error_retryable",
    )
    events = list(gateway.fulfill(call))
    error = next(event for event in events if event.kind == intelligence_pb2.ModelEvent.KIND_ERROR)
    assert error.error.code == "PROVIDER_RATE_LIMITED"
    assert CANARY_CREDENTIAL not in error.error.code
    assert CANARY_PROMPT not in error.error.code
