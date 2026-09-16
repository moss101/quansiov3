"""EXEC-011 adapters: contract conformance, token isolation, webhook replay.

The suite drives the shipped `run_suite` over all four GA adapters through the
contract tier (offline), asserts the sandbox tier reports BLOCKED_EXTERNAL when
its flag is absent, and proves webhook redelivery is idempotent and tenant
scoped. The in-sandbox tier executes only under `QUANSIO_TEST_CONNECTOR_*=1`.
"""

from __future__ import annotations

import pytest

from intelligence.adapters.connectors import (
    GA_ADAPTERS,
    GITHUB,
    GITHUB_CI,
    GITHUB_PR,
    WEB_SEARCH,
    AdapterError,
    WebhookReplay,
    run_suite,
    sandbox_flag,
)

RAW_TOKEN = "gho_rawTokenMaterialMustNeverAppear"


def test_all_ga_adapters_pass_the_contract_tier() -> None:
    """Named test: the shared conformance suite, contract tier, all four adapters."""
    for adapter in GA_ADAPTERS:
        results = {item.tier: item for item in run_suite([adapter], sandbox_transport=object())}
        contract = results["contract"]
        assert contract.passed, f"{adapter.connector_id}: {contract.detail}"
    # And the sandbox tier reported BLOCKED_EXTERNAL, never a silent skip.
    for adapter in GA_ADAPTERS:
        results = {item.tier: item for item in run_suite([adapter], sandbox_transport=object())}
        sandbox = results["sandbox"]
        assert not sandbox.passed
        assert sandbox.detail.startswith("BLOCKED_EXTERNAL"), sandbox.detail
        assert sandbox_flag(adapter) in sandbox.detail


def test_operations_are_closed_and_consequential_flags_declared() -> None:
    """Unknown ops are refused; every GA surface declares its effect class."""
    with pytest.raises(AdapterError):
        GITHUB.operation("repo.force_push")
    consequential = {op.name for adapter in GA_ADAPTERS for op in adapter.operations if op.consequential}
    assert {"issue.comment", "gmail.send", "calendar.insert", "chat.postMessage"} <= consequential
    reads = {op.name for adapter in GA_ADAPTERS for op in adapter.operations if not op.consequential}
    assert {"repo.read", "gmail.list", "drive.list", "calendar.list", "channels.history", "search"} <= reads


def test_web_search_is_the_websearch_adapter_surface() -> None:
    """`web.search` is served by the websearch adapter's `search` op."""
    assert WEB_SEARCH.connector_id == "websearch"
    assert [op.name for op in WEB_SEARCH.operations] == ["search"]
    assert all(not op.consequential for op in WEB_SEARCH.operations)


def test_token_isolation_refuses_raw_material_everywhere() -> None:
    """Named test: token isolation. Raw tokens are refused at every adapter seam."""
    for adapter in GA_ADAPTERS:
        op = adapter.operations[0]
        args = dict.fromkeys(op.required, "x")
        with pytest.raises(AdapterError) as raised:
            adapter.build_request(op.name, args, RAW_TOKEN)
        assert "sec_ handles" in raised.value.detail
        # And a valid handle flows through as an opaque header, never a token.
        _op, _method, url, headers, _body = adapter.build_request(
            op.name, args, "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
        )
        assert headers["X-Quansio-Credential-Handle"].startswith("sec_")
        assert RAW_TOKEN not in url + str(headers)


def test_webhook_redelivery_is_idempotent_and_tenant_scoped() -> None:
    """Named test: webhook replay. Same event id replays once; tenants don't collide."""
    replay = WebhookReplay()
    assert replay.deliver(tenant_id="tn_a", event_id="evt_1", payload={"n": 1}) is True
    assert replay.deliver(tenant_id="tn_a", event_id="evt_1", payload={"n": 1}) is False, (
        "replay delivered twice"
    )
    # Another tenant's identical event id is its own delivery.
    assert replay.deliver(tenant_id="tn_b", event_id="evt_1", payload={"n": 2}) is True
    assert len(replay.log) == 2
    assert {tenant for tenant, _, _ in replay.log} == {"tn_a", "tn_b"}


def test_pr_and_ci_adapters_pass_the_contract_tier() -> None:
    """Named test (EXEC-012): the PR/CI adapters clear the same shared suite."""
    for adapter in (GITHUB_PR, GITHUB_CI):
        results = {item.tier: item for item in run_suite([adapter], sandbox_transport=object())}
        assert results["contract"].passed, f"{adapter.connector_id}: {results['contract'].detail}"
        sandbox = results["sandbox"]
        assert not sandbox.passed
        assert sandbox.detail.startswith("BLOCKED_EXTERNAL"), sandbox.detail
        assert sandbox_flag(adapter) in sandbox.detail

    # Every PR/CI write is consequential and therefore approval-gated by policy.
    writes = {op.name for adapter in (GITHUB_PR, GITHUB_CI) for op in adapter.operations if op.consequential}
    assert {"pr.create", "pr.comment", "pr.merge", "ci.rerun"} <= writes
    # Reads are not consequential.
    assert not GITHUB_CI.operation("ci.runs").consequential
    assert not GITHUB_PR.operation("pr.read").consequential
