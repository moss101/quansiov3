"""Offline conformance infrastructure for the model gateway.

`stub_provider` is a deterministic loopback provider (Anthropic and OpenAI wire formats);
`suite` is the shared conformance suite that every adapter runs. Neither is real-boundary
evidence: only the `QUANSIO_TEST_*`-gated live suite can be that.
"""

from __future__ import annotations

from intelligence.model_gateway.conformance.stub_provider import (
    SCENARIOS,
    NonRespondingProvider,
    RecordedCall,
    StubProvider,
    StubState,
)
from intelligence.model_gateway.conformance.suite import (
    CONFORMANCE_CREDENTIAL_HANDLE,
    CONFORMANCE_CREDENTIAL_VALUE,
    CONFORMANCE_TOOL_ARGS,
    CONFORMANCE_TOOL_NAME,
    LIVE_MODE,
    LIVE_PROVIDER_KINDS,
    LIVE_SCENARIOS,
    SCENARIO_EXPECTATIONS,
    STUB_MODE,
    STUB_PROVIDER_KINDS,
    STUB_SCENARIOS,
    ConformanceReport,
    ScenarioExpectation,
    VerificationMode,
    build_call,
    catalog_model_for_kind,
    conformance_tool_schemas,
    credential_environ,
    run_scenario,
    stub_catalog,
    verify,
)

__all__ = [
    "CONFORMANCE_CREDENTIAL_HANDLE",
    "CONFORMANCE_CREDENTIAL_VALUE",
    "CONFORMANCE_TOOL_ARGS",
    "CONFORMANCE_TOOL_NAME",
    "LIVE_MODE",
    "LIVE_PROVIDER_KINDS",
    "LIVE_SCENARIOS",
    "SCENARIOS",
    "SCENARIO_EXPECTATIONS",
    "STUB_MODE",
    "STUB_PROVIDER_KINDS",
    "STUB_SCENARIOS",
    "ConformanceReport",
    "NonRespondingProvider",
    "RecordedCall",
    "ScenarioExpectation",
    "StubProvider",
    "StubState",
    "VerificationMode",
    "build_call",
    "catalog_model_for_kind",
    "conformance_tool_schemas",
    "credential_environ",
    "run_scenario",
    "stub_catalog",
    "verify",
]
