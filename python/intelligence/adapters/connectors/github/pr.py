"""GitHub PR adapter (EXEC-012): create, comment, merge — all consequential writes."""

from __future__ import annotations

from intelligence.adapters.connectors.base import ConnectorAdapter, Operation

GITHUB_PR = ConnectorAdapter(
    connector_id="github-pr",
    base_url="https://api.github.com",
    authorize_url="https://github.com/login/oauth/authorize",
    scope="repo,pull_requests:write",
    extra_headers={"X-GitHub-Api-Version": "2022-11-28"},
    operations=(
        Operation(
            name="pr.read",
            method="GET",
            path="/repos/{owner}/{repo}/pulls/{number}",
            consequential=False,
            required=("owner", "repo", "number"),
        ),
        Operation(
            name="pr.create",
            method="POST",
            path="/repos/{owner}/{repo}/pulls",
            consequential=True,
            required=("owner", "repo"),
        ),
        Operation(
            name="pr.comment",
            method="POST",
            path="/repos/{owner}/{repo}/issues/{number}/comments",
            consequential=True,
            required=("owner", "repo", "number"),
        ),
        Operation(
            name="pr.merge",
            method="PUT",
            path="/repos/{owner}/{repo}/pulls/{number}/merge",
            consequential=True,
            required=("owner", "repo", "number"),
        ),
    ),
)
