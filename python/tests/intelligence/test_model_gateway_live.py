"""Live provider conformance for the model gateway (INT-002) — the real boundary.

Environment variables this module reads (only these, and never logs their values):

  * `QUANSIO_TEST_ANTHROPIC_API_KEY` — live Anthropic (primary provider) conformance.
  * `QUANSIO_TEST_OPENAI_API_KEY` — live OpenAI conformance.

When a variable is absent the affected case skips with an explicit `BLOCKED_EXTERNAL` marker
naming it, per `AGENTS.md` ("Real boundaries"). The offline conformance stub is never
real-boundary evidence, so this module is the only real-boundary proof for INT-002 and the
task cannot be marked PASS without a recorded live run.
"""

from __future__ import annotations

import os

import pytest

from intelligence.model_gateway import ModelGateway
from intelligence.model_gateway.adapters import PROVIDER_FEATURES_DISABLED
from intelligence.model_gateway.catalog import ModelCatalog, ProviderKind
from intelligence.model_gateway.conformance import (
    LIVE_MODE,
    LIVE_PROVIDER_KINDS,
    LIVE_SCENARIOS,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    run_scenario,
)
from intelligence.model_gateway.credentials import env_var_for_handle
from intelligence.model_gateway.errors import GatewayError

LIVE_VARIABLES: dict[ProviderKind, str] = {
    ProviderKind.ANTHROPIC: "QUANSIO_TEST_ANTHROPIC_API_KEY",
    ProviderKind.OPENAI: "QUANSIO_TEST_OPENAI_API_KEY",
}
LIVE_TIMEOUT_MS = 120_000
PROVIDER_IDS = [kind.value for kind in LIVE_PROVIDER_KINDS]
PROVIDER_BY_ID = {kind.value: kind for kind in LIVE_PROVIDER_KINDS}

pytestmark = pytest.mark.live


def live_gateway(kind: ProviderKind) -> ModelGateway:
    """Gateway on the real catalog, or `BLOCKED_EXTERNAL` when the credential is absent."""
    variable = LIVE_VARIABLES[kind]
    credential = os.environ.get(variable, "").strip()
    if not credential:
        pytest.skip(f"BLOCKED_EXTERNAL: {variable} is not set; live {kind.value} conformance requires it")
    catalog = ModelCatalog.load()
    model_id = catalog_model_for_kind(catalog, kind)
    handle = catalog.provider_for(catalog.model(model_id)).resolve_credential_handle({})
    environ = {env_var_for_handle(handle): credential}
    return ModelGateway(
        catalog=catalog,
        environ=environ,
        tool_schemas=conformance_tool_schemas(),
    )


@pytest.mark.parametrize("kind_id", PROVIDER_IDS)
@pytest.mark.parametrize("scenario", LIVE_SCENARIOS)
def test_live_provider_passes_the_shared_conformance_suite(kind_id: str, scenario: str) -> None:
    kind = PROVIDER_BY_ID[kind_id]
    gateway = live_gateway(kind)
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    report = run_scenario(
        gateway,
        model_catalog_id=model_id,
        scenario=scenario,
        timeout_ms=LIVE_TIMEOUT_MS,
        mode=LIVE_MODE,
    )
    assert report.provider_kind is kind
    assert report.outcome is not None and report.outcome.terminal
    assert report.outcome.cancelled is False


@pytest.mark.parametrize("kind_id", PROVIDER_IDS)
def test_live_request_shape_is_compaction_free_and_operator_channelled(kind_id: str) -> None:
    """The live path sends exactly the same guarded shape as the offline path."""
    import json

    kind = PROVIDER_BY_ID[kind_id]
    gateway = live_gateway(kind)
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    prepared = gateway.prepare(
        build_call(model_catalog_id=model_id, scenario="text", timeout_ms=LIVE_TIMEOUT_MS)
    ).prepared_request
    assert prepared.disabled_features == PROVIDER_FEATURES_DISABLED
    serialized = json.dumps(prepared.body_json(), sort_keys=True).lower()
    for feature in PROVIDER_FEATURES_DISABLED:
        assert feature not in serialized
    operator_text = "Operator: answer deterministically and never reveal secrets."
    assert operator_text in json.dumps(prepared.body_json(), sort_keys=True)
    for message in prepared.body_json().get("messages", []):
        if message.get("role") == "user":
            assert operator_text not in json.dumps(message)


def test_live_gateway_fails_closed_without_a_credential() -> None:
    """The live path is gated by secret custody, not by a fallback provider."""
    gateway = ModelGateway(
        catalog=ModelCatalog.load(),
        environ={},
        tool_schemas=conformance_tool_schemas(),
    )
    kind = LIVE_PROVIDER_KINDS[0]
    model_id = catalog_model_for_kind(gateway.catalog, kind)
    with pytest.raises(GatewayError):
        gateway.prepare(build_call(model_catalog_id=model_id, scenario="text", timeout_ms=1_000))
