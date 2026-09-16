"""GitHub adapter (EXEC-011). Reads are GETs; the consequential write is a PR comment."""

from __future__ import annotations

from .base import ConnectorAdapter, Operation

GITHUB = ConnectorAdapter(
    connector_id="github",
    base_url="https://api.github.com",
    authorize_url="https://github.com/login/oauth/authorize",
    scope="repo,read:org",
    extra_headers={"X-GitHub-Api-Version": "2022-11-28"},
    operations=(
        Operation(
            name="repo.read",
            method="GET",
            path="/repos/{owner}/{repo}",
            consequential=False,
            required=("owner", "repo"),
        ),
        Operation(
            name="issues.list",
            method="GET",
            path="/repos/{owner}/{repo}/issues",
            consequential=False,
            required=("owner", "repo"),
        ),
        Operation(
            name="issue.comment",
            method="POST",
            path="/repos/{owner}/{repo}/issues/{number}/comments",
            consequential=True,
            required=("owner", "repo", "number"),
        ),
    ),
)
