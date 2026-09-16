"""Connector adapters (EXEC-011): the GA providers behind `connector.<id>.<op>`.

The broker (`crates/server/src/control/connectors/`) owns metadata, OAuth and the
lifecycle; these adapters own the provider request/response shapes and are gated
by the shared conformance suite (`conformance.run_suite`).
"""

from __future__ import annotations

from .base import HANDLE_PREFIX, AdapterError, ConnectorAdapter, HttpTransport, Operation, require_handle
from .conformance import TierResult, run_contract_tier, run_sandbox_tier, run_suite, sandbox_flag
from .github import GITHUB, GITHUB_CI, GITHUB_PR
from .google import GOOGLE
from .slack import SLACK
from .webhooks import WebhookReplay
from .websearch import WEB_SEARCH

GA_ADAPTERS: tuple[ConnectorAdapter, ...] = (GITHUB, GOOGLE, SLACK, WEB_SEARCH)

__all__ = [
    "GA_ADAPTERS",
    "GITHUB",
    "GITHUB_CI",
    "GITHUB_PR",
    "GOOGLE",
    "HANDLE_PREFIX",
    "SLACK",
    "WEB_SEARCH",
    "AdapterError",
    "ConnectorAdapter",
    "HttpTransport",
    "Operation",
    "TierResult",
    "WebhookReplay",
    "require_handle",
    "run_contract_tier",
    "run_sandbox_tier",
    "run_suite",
    "sandbox_flag",
]
