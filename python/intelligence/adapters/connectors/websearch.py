"""Web-search adapter (EXEC-011): the provider behind the `web.search` tool."""

from __future__ import annotations

from .base import ConnectorAdapter, Operation

WEB_SEARCH = ConnectorAdapter(
    connector_id="websearch",
    base_url="https://api.search-provider.example",
    authorize_url="https://search-provider.example/oauth/authorize",
    scope="search",
    operations=(
        Operation(
            name="search",
            method="GET",
            path="/v1/search",
            consequential=False,
        ),
    ),
)
