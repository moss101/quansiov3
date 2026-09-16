"""Slack adapter (EXEC-011): channel reads and consequential message writes."""

from __future__ import annotations

from .base import ConnectorAdapter, Operation

SLACK = ConnectorAdapter(
    connector_id="slack",
    base_url="https://slack.com/api",
    authorize_url="https://slack.com/oauth/authorize",
    scope="channels:history,chat:write",
    operations=(
        Operation(
            name="channels.history",
            method="GET",
            path="/conversations.history",
            consequential=False,
        ),
        Operation(
            name="chat.postMessage",
            method="POST",
            path="/chat.postMessage",
            consequential=True,
        ),
    ),
)
