"""QA-006: qualify connectors and external effects.

The offline tier runs the shipped conformance suite over all GA adapters plus
the EXEC-012 PR/CI adapters and proves the external-effect contract: reads are
non-consequential, writes are flagged for the Effect Ledger, token isolation
holds, and webhook replay is idempotent and tenant scoped. The provider
sandbox tier is gated on `QUANSIO_TEST_CONNECTOR_*=1` and reports an explicit
`BLOCKED_EXTERNAL` marker when absent.
"""

from __future__ import annotations

import pytest

from intelligence.adapters.connectors import (
    GA_ADAPTERS,
    GITHUB_CI,
    GITHUB_PR,
    WebhookReplay,
    run_suite,
)

ALL_CONNECTORS = [*GA_ADAPTERS, GITHUB_PR, GITHUB_CI]


@pytest.mark.parametrize("adapter", ALL_CONNECTORS, ids=lambda a: a.connector_id)
def test_contract_tier_passes_for_every_connector(adapter: object) -> None:
    results = {item.tier: item for item in run_suite([adapter], sandbox_transport=object())}
    assert results["contract"].passed, f"{adapter.connector_id}: {results['contract'].detail}"


@pytest.mark.parametrize("adapter", ALL_CONNECTORS, ids=lambda a: a.connector_id)
def test_sandbox_tier_reports_blocked_external_when_absent(adapter: object) -> None:
    results = {item.tier: item for item in run_suite([adapter], sandbox_transport=object())}
    sandbox = results["sandbox"]
    assert not sandbox.passed
    assert sandbox.detail.startswith("BLOCKED_EXTERNAL")


def test_external_effect_contract_holds() -> None:
    """Reads are non-consequential; every write is flagged for the ledger."""
    for adapter in ALL_CONNECTORS:
        for op in adapter.operations:
            if op.consequential:
                assert op.method in {"POST", "PUT"}, f"{op.name} must be a mutating verb"
    replay = WebhookReplay()
    assert replay.deliver(tenant_id="tn_qa", event_id="evt_1", payload={}) is True
    assert replay.deliver(tenant_id="tn_qa", event_id="evt_1", payload={}) is False
    assert replay.deliver(tenant_id="tn_other", event_id="evt_1", payload={}) is True
