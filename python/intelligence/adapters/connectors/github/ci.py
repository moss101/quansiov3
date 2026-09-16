"""GitHub Actions CI adapter (EXEC-012): workflow-run reads and an explicit rerun."""

from __future__ import annotations

from intelligence.adapters.connectors.base import ConnectorAdapter, Operation

GITHUB_CI = ConnectorAdapter(
    connector_id="github-ci",
    base_url="https://api.github.com",
    authorize_url="https://github.com/login/oauth/authorize",
    scope="repo,actions:read",
    extra_headers={"X-GitHub-Api-Version": "2022-11-28"},
    operations=(
        Operation(
            name="ci.runs",
            method="GET",
            path="/repos/{owner}/{repo}/actions/runs",
            consequential=False,
            required=("owner", "repo"),
        ),
        Operation(
            name="ci.run",
            method="GET",
            path="/repos/{owner}/{repo}/actions/runs/{run_id}",
            consequential=False,
            required=("owner", "repo", "run_id"),
        ),
        Operation(
            name="ci.rerun",
            method="POST",
            path="/repos/{owner}/{repo}/actions/runs/{run_id}/rerun",
            consequential=True,
            required=("owner", "repo", "run_id"),
        ),
    ),
)
