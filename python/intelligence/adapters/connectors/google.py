"""Google Workspace adapter (EXEC-011): Gmail read/send, Drive, Calendar."""

from __future__ import annotations

from .base import ConnectorAdapter, Operation

GOOGLE = ConnectorAdapter(
    connector_id="google",
    base_url="https://www.googleapis.com",
    authorize_url="https://accounts.google.com/o/oauth2/v2/auth",
    scope="gmail.readonly gmail.send drive.readonly calendar.events",
    extra_headers={"X-Goog-Api-Client": "quansio-connector"},
    operations=(
        Operation(
            name="gmail.list",
            method="GET",
            path="/gmail/v1/users/{user}/messages",
            consequential=False,
            required=("user",),
        ),
        Operation(
            name="gmail.send",
            method="POST",
            path="/gmail/v1/users/{user}/messages/send",
            consequential=True,
            required=("user",),
        ),
        Operation(
            name="drive.list",
            method="GET",
            path="/drive/v3/files",
            consequential=False,
        ),
        Operation(
            name="calendar.list",
            method="GET",
            path="/calendar/v3/users/{user}/events",
            consequential=False,
            required=("user",),
        ),
        Operation(
            name="calendar.insert",
            method="POST",
            path="/calendar/v3/users/{user}/events",
            consequential=True,
            required=("user",),
        ),
    ),
)
