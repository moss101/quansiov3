"""Live provider conformance for the model gateway (INT-002) — the real boundary.

DOSSIER.md §2.1 requires that at least two of the three adapters pass the live conformance
suite; it does not say which two. Every adapter therefore has a live boundary here, and the
acceptance test counts the ones that pass.

Environment variables this module reads (only these, and never logs their values):

  * `QUANSIO_TEST_ANTHROPIC_API_KEY` — live Anthropic (primary provider) conformance.
  * `QUANSIO_TEST_OPENAI_API_KEY` — live OpenAI conformance.
  * `QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL`, `QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY` and
    `QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID` — live conformance of the generic adapter against
    any endpoint speaking the OpenAI chat-completions wire format with streaming and tool
    calling (vLLM, llama.cpp, Ollama, LM Studio, TGI, or a hosted service). The base URL is the
    endpoint root without a version segment (`http://127.0.0.1:11434`, not `.../v1`). The
    model id is the name that endpoint serves, because a self-hosted model's name is
    deployment data like its base URL; the catalog entry's capabilities and policy still come
    from `config/models.yaml`. An endpoint that does not authenticate still needs a non-empty
    placeholder key, since the gateway fails closed without a materialized credential.

When a variable is absent the affected case skips with an explicit `BLOCKED_EXTERNAL` marker
naming it, per `AGENTS.md` ("Real boundaries"). The offline conformance stub is never
real-boundary evidence, so this module is the only real-boundary proof for INT-002 and the
task cannot be marked PASS without a recorded live run in which at least two providers pass.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass

import pytest

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.adapters import PROVIDER_FEATURES_DISABLED
from intelligence.model_gateway.catalog import ModelCatalog, ProviderKind
from intelligence.model_gateway.conformance import (
    LIVE_MODE,
    LIVE_PROVIDER_KINDS,
    LIVE_PROVIDERS_REQUIRED,
    LIVE_SCENARIOS,
    StubProvider,
    build_call,
    catalog_model_for_kind,
    catalog_with_wire_model_id,
    conformance_tool_schemas,
    run_scenario,
)
from intelligence.model_gateway.credentials import env_var_for_handle
from intelligence.model_gateway.errors import GatewayError

OPENAI_COMPATIBLE_BASE_URL_VARIABLE = "QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL"
OPENAI_COMPATIBLE_API_KEY_VARIABLE = "QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY"
OPENAI_COMPATIBLE_MODEL_ID_VARIABLE = "QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID"
# The handle the test materializes for the generic provider; the catalog only names the
# variable that carries the handle, so the test supplies one of its own.
OPENAI_COMPATIBLE_CREDENTIAL_HANDLE = "provider/openai-compatible"

LIVE_VARIABLES: dict[ProviderKind, tuple[str, ...]] = {
    ProviderKind.ANTHROPIC: ("QUANSIO_TEST_ANTHROPIC_API_KEY",),
    ProviderKind.OPENAI: ("QUANSIO_TEST_OPENAI_API_KEY",),
    ProviderKind.OPENAI_COMPATIBLE: (
        OPENAI_COMPATIBLE_BASE_URL_VARIABLE,
        OPENAI_COMPATIBLE_API_KEY_VARIABLE,
        OPENAI_COMPATIBLE_MODEL_ID_VARIABLE,
    ),
}
LIVE_TIMEOUT_MS = 120_000
PROVIDER_IDS = [kind.value for kind in LIVE_PROVIDER_KINDS]
PROVIDER_BY_ID = {kind.value: kind for kind in LIVE_PROVIDER_KINDS}

pytestmark = pytest.mark.live


@dataclass(frozen=True, slots=True)
class LiveBoundary:
    """One provider's live configuration: the catalog to route through and the custody environ."""

    kind: ProviderKind
    catalog: ModelCatalog
    environ: Mapping[str, str]

    @property
    def model_id(self) -> str:
        return catalog_model_for_kind(self.catalog, self.kind)

    def gateway(self) -> ModelGateway:
        return ModelGateway(
            catalog=self.catalog,
            environ=self.environ,
            tool_schemas=conformance_tool_schemas(),
        )


def missing_variables(kind: ProviderKind, environ: Mapping[str, str] | None = None) -> tuple[str, ...]:
    """The `QUANSIO_TEST_*` variables `kind` needs that are unset or blank."""
    source = os.environ if environ is None else environ
    return tuple(name for name in LIVE_VARIABLES[kind] if not source.get(name, "").strip())


def live_boundary(kind: ProviderKind, environ: Mapping[str, str] | None = None) -> LiveBoundary:
    """The live configuration for `kind`, built only from its `QUANSIO_TEST_*` variables.

    Raises `LookupError` naming the missing variables when the boundary is not configured, so
    a test can skip with the `BLOCKED_EXTERNAL` marker and an acceptance check can count.
    """
    source = os.environ if environ is None else environ
    missing = missing_variables(kind, source)
    if missing:
        raise LookupError(", ".join(missing))
    values = {name: source[name].strip() for name in LIVE_VARIABLES[kind]}
    if kind is ProviderKind.OPENAI_COMPATIBLE:
        catalog = catalog_with_wire_model_id(kind, values[OPENAI_COMPATIBLE_MODEL_ID_VARIABLE])
        provider = catalog.provider_for(catalog.model(catalog_model_for_kind(catalog, kind)))
        assert provider.base_url_env and provider.credential_handle_env, (
            "the OpenAI-compatible provider is configured by indirection only (OD-004)"
        )
        gateway_environ = {
            provider.base_url_env: values[OPENAI_COMPATIBLE_BASE_URL_VARIABLE],
            provider.credential_handle_env: OPENAI_COMPATIBLE_CREDENTIAL_HANDLE,
            env_var_for_handle(OPENAI_COMPATIBLE_CREDENTIAL_HANDLE): values[
                OPENAI_COMPATIBLE_API_KEY_VARIABLE
            ],
        }
        return LiveBoundary(kind=kind, catalog=catalog, environ=gateway_environ)
    catalog = ModelCatalog.load()
    handle = catalog.provider_for(
        catalog.model(catalog_model_for_kind(catalog, kind))
    ).resolve_credential_handle({})
    (credential,) = values.values()
    return LiveBoundary(kind=kind, catalog=catalog, environ={env_var_for_handle(handle): credential})


def live_gateway(kind: ProviderKind) -> LiveBoundary:
    """The live boundary for `kind`, or `BLOCKED_EXTERNAL` when its variables are absent."""
    try:
        return live_boundary(kind)
    except LookupError as missing:
        pytest.skip(f"BLOCKED_EXTERNAL: {missing} is not set; live {kind.value} conformance requires it")


@pytest.mark.parametrize("kind_id", PROVIDER_IDS)
@pytest.mark.parametrize("scenario", LIVE_SCENARIOS)
def test_live_provider_passes_the_shared_conformance_suite(kind_id: str, scenario: str) -> None:
    kind = PROVIDER_BY_ID[kind_id]
    boundary = live_gateway(kind)
    report = run_scenario(
        boundary.gateway(),
        model_catalog_id=boundary.model_id,
        scenario=scenario,
        timeout_ms=LIVE_TIMEOUT_MS,
        mode=LIVE_MODE,
    )
    assert report.provider_kind is kind
    assert report.outcome is not None and report.outcome.terminal
    assert report.outcome.cancelled is False


def test_at_least_two_live_providers_pass_the_same_suite() -> None:
    """Acceptance: at least two provider adapters pass the same conformance suite, live.

    Skips with `BLOCKED_EXTERNAL` naming every unconfigured provider's variables while fewer
    than two providers are configured; with two or more, every configured provider runs the
    live scenarios and the count of fully passing ones must reach the requirement.
    """
    configured: dict[ProviderKind, LiveBoundary] = {}
    unconfigured: list[str] = []
    for kind in LIVE_PROVIDER_KINDS:
        try:
            configured[kind] = live_boundary(kind)
        except LookupError as missing:
            unconfigured.append(f"{kind.value} ({missing})")
    if len(configured) < LIVE_PROVIDERS_REQUIRED:
        pytest.skip(
            f"BLOCKED_EXTERNAL: {len(configured)} of {len(LIVE_PROVIDER_KINDS)} live providers "
            f"configured; at least {LIVE_PROVIDERS_REQUIRED} are required. Unconfigured: "
            + "; ".join(unconfigured)
        )
    passed: dict[str, list[str]] = {}
    for kind, boundary in configured.items():
        gateway = boundary.gateway()
        passed[kind.value] = [
            scenario for scenario in LIVE_SCENARIOS if _passes(gateway, boundary.model_id, scenario)
        ]
    qualifying = {
        kind: scenarios for kind, scenarios in passed.items() if set(scenarios) == set(LIVE_SCENARIOS)
    }
    assert len(qualifying) >= LIVE_PROVIDERS_REQUIRED, (
        f"fewer than {LIVE_PROVIDERS_REQUIRED} live providers passed the same suite: {passed}"
    )


@pytest.mark.parametrize("kind_id", PROVIDER_IDS)
def test_live_request_shape_is_compaction_free_and_operator_channelled(kind_id: str) -> None:
    """The live path sends exactly the same guarded shape as the offline path."""
    import json

    kind = PROVIDER_BY_ID[kind_id]
    boundary = live_gateway(kind)
    prepared = (
        boundary.gateway()
        .prepare(build_call(model_catalog_id=boundary.model_id, scenario="text", timeout_ms=LIVE_TIMEOUT_MS))
        .prepared_request
    )
    assert prepared.disabled_features == PROVIDER_FEATURES_DISABLED
    serialized = json.dumps(prepared.body_json(), sort_keys=True).lower()
    for feature in PROVIDER_FEATURES_DISABLED:
        assert feature not in serialized
    operator_text = "Operator: answer deterministically and never reveal secrets."
    assert operator_text in json.dumps(prepared.body_json(), sort_keys=True)
    for message in prepared.body_json().get("messages", []):
        if message.get("role") == "user":
            assert operator_text not in json.dumps(message)


@pytest.mark.parametrize("kind_id", PROVIDER_IDS)
def test_live_gateway_fails_closed_without_a_credential(kind_id: str) -> None:
    """The live path is gated by secret custody, not by a fallback provider."""
    kind = PROVIDER_BY_ID[kind_id]
    gateway = ModelGateway(
        catalog=ModelCatalog.load(),
        environ={},
        tool_schemas=conformance_tool_schemas(),
    )
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    with pytest.raises(GatewayError):
        gateway.prepare(build_call(model_catalog_id=model_id, scenario="text", timeout_ms=1_000))


def test_openai_compatible_boundary_is_built_only_from_its_test_variables() -> None:
    """The generic adapter's live configuration comes from `QUANSIO_TEST_*` and the catalog alone.

    Offline check of the wiring the live case relies on: the endpoint, the credential and the
    served model name flow from the three test variables into the catalog's own indirection
    variables and secret handle, without touching `config/models.yaml` or the process environment.
    """
    environ = {
        OPENAI_COMPATIBLE_BASE_URL_VARIABLE: "http://127.0.0.1:11434/",
        OPENAI_COMPATIBLE_API_KEY_VARIABLE: "not-a-credential",
        OPENAI_COMPATIBLE_MODEL_ID_VARIABLE: "served-model-name",
    }
    boundary = live_boundary(ProviderKind.OPENAI_COMPATIBLE, environ)
    spec = boundary.catalog.model(boundary.model_id)
    provider = boundary.catalog.provider_for(spec)
    committed = ModelCatalog.load().model(boundary.model_id)
    assert spec.model_id == "served-model-name"
    assert spec.model_id != committed.model_id
    assert (spec.capabilities, spec.context_window, spec.cost_class, spec.dlp_eligible) == (
        committed.capabilities,
        committed.context_window,
        committed.cost_class,
        committed.dlp_eligible,
    )
    assert provider.resolve_base_url(boundary.environ) == "http://127.0.0.1:11434"
    assert provider.resolve_credential_handle(boundary.environ) == OPENAI_COMPATIBLE_CREDENTIAL_HANDLE
    prepared = (
        boundary.gateway()
        .prepare(build_call(model_catalog_id=boundary.model_id, scenario="text", timeout_ms=1_000))
        .prepared_request
    )
    assert prepared.body_json()["model"] == "served-model-name"
    assert "not-a-credential" not in repr(prepared)
    assert (
        missing_variables(ProviderKind.OPENAI_COMPATIBLE, {})
        == LIVE_VARIABLES[ProviderKind.OPENAI_COMPATIBLE]
    )
    with pytest.raises(LookupError, match=OPENAI_COMPATIBLE_MODEL_ID_VARIABLE):
        live_boundary(
            ProviderKind.OPENAI_COMPATIBLE,
            {name: value for name, value in environ.items() if name != OPENAI_COMPATIBLE_MODEL_ID_VARIABLE},
        )


def test_openai_compatible_live_wiring_reaches_the_endpoint_the_variables_name() -> None:
    """The live path for the generic adapter, driven end to end through its three variables.

    Offline: the loopback stub speaks the OpenAI chat-completions wire format, so it stands in
    for the endpoint only to prove the wiring — the base URL, the credential and the served
    model name all reach the wire from `QUANSIO_TEST_OPENAI_COMPATIBLE_*` alone. It is not
    real-boundary evidence (AGENTS.md); only a run against a real endpoint is.
    """
    with StubProvider() as stub:
        boundary = live_boundary(
            ProviderKind.OPENAI_COMPATIBLE,
            {
                OPENAI_COMPATIBLE_BASE_URL_VARIABLE: stub.base_url,
                OPENAI_COMPATIBLE_API_KEY_VARIABLE: "not-a-credential",
                OPENAI_COMPATIBLE_MODEL_ID_VARIABLE: "served-model-name",
            },
        )
        gateway = boundary.gateway()
        for scenario in LIVE_SCENARIOS:
            before = len(stub.state.calls)
            report = run_scenario(
                gateway,
                model_catalog_id=boundary.model_id,
                scenario=scenario,
                timeout_ms=LIVE_TIMEOUT_MS,
                mode=LIVE_MODE,
            )
            assert report.provider_kind is ProviderKind.OPENAI_COMPATIBLE
            call = stub.state.calls[before]
            assert call.path == "/v1/chat/completions"
            assert call.json()["model"] == "served-model-name"
            assert call.headers.get("authorization") == "Bearer not-a-credential"


def _passes(gateway: ModelGateway, model_id: str, scenario: str) -> bool:
    try:
        run_scenario(
            gateway, model_catalog_id=model_id, scenario=scenario, timeout_ms=LIVE_TIMEOUT_MS, mode=LIVE_MODE
        )
    except (AssertionError, GatewayError):
        return False
    return True
