"""SDK compatibility against the current server, Python (APP-013).

`quansio-server` is started on a scratch database and the generated operation table
(`sdk/python/quansio_sdk/generated/operations.py`) is checked against what it serves; then
a session is obtained through the SDK from the magic link on the server's console and a
command and a projection go through it; a refused command is a typed, catalogued error.
The pure half (paths, envelope, error typing) runs without a server. Blocked, not
skipped, without the dev stack or the built binaries.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import threading
import time
from collections.abc import Iterator
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "sdk" / "python"))

from quansio_sdk import (
    ERROR_CODES,
    OPERATIONS,
    READ_PROJECTIONS,
    ApiError,
    Client,
    Credential,
    new_command_id,
    public_api_path,
)

ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()
SERVER_BIN = Path(
    os.environ.get("QUANSIO_TEST_SERVER_BIN")
    or ROOT / "target" / "debug" / "quansio-server"
)
SEED_BIN = Path(
    os.environ.get("QUANSIO_TEST_SEED_BIN")
    or ROOT / "target" / "debug" / "quansio-dev-seed"
)


def test_the_operation_table_is_the_openapi_documents_and_paths_are_versioned() -> None:
    openapi = (ROOT / "schemas" / "openapi" / "public-api-v1.yaml").read_text(
        encoding="utf-8"
    )
    documented = sorted(
        line.strip()[len("/v1/commands/") : -1]
        for line in openapi.splitlines()
        if line.startswith("  /v1/commands/") and line.endswith(":")
    )
    assert sorted(OPERATIONS) == documented
    for name, entry in OPERATIONS.items():
        assert entry["path"] == f"/v1/commands/{name}"
        assert entry["method"] == "POST"
        assert f"operationId: {name}" in openapi
    catalog = (ROOT / "schemas" / "catalog" / "read-projections.yaml").read_text(
        encoding="utf-8"
    )
    for projection in READ_PROJECTIONS:
        assert f"- {projection}" in catalog
    errors = (ROOT / "schemas" / "catalog" / "errors.yaml").read_text(encoding="utf-8")
    assert all(code in errors for code in ERROR_CODES)
    assert public_api_path("runs") == "/v1/runs"
    assert public_api_path(
        "messages?thread_id=thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"
    ).startswith("/v1/messages?")
    for bad in ["", "/runs", "../admin", "runs/../x", "https://x/v1/a", "a b"]:
        with pytest.raises(ValueError):
            public_api_path(bad)
    assert new_command_id().startswith("cmd_") and len(new_command_id()) == 30
    assert new_command_id() != new_command_id()


def test_the_client_sends_the_envelope_and_types_the_error() -> None:
    calls: list[tuple[str, str, dict[str, str], bytes | None]] = []

    def transport(
        method: str, url: str, headers: dict[str, str], body: bytes | None
    ) -> tuple[int, bytes]:
        calls.append((method, url, headers, body))
        if url.endswith("/v1/audit"):
            return 403, json.dumps(
                {
                    "code": "SCOPE_FORBIDDEN",
                    "message": "no",
                    "correlation_id": "c1",
                    "retryable": False,
                    "details": {"kind": "rbac"},
                }
            ).encode()
        return 200, json.dumps(
            {"command_id": "cmd_x", "replayed": False, "result": {"url": url}}
        ).encode()

    client = Client(
        "https://q.example/",
        Credential(access_token=lambda: "tok"),
        transport=transport,
    )
    result = client.command("PostMessage", {"workspace_id": "ws_1", "content": "hi"})
    assert result == {"url": "https://q.example/v1/commands/PostMessage"}
    method, _, headers, body = calls[0]
    assert method == "POST"
    assert headers["Authorization"] == "Bearer tok"
    assert headers["x-quansio-client"] == "sdk-python"
    envelope = json.loads(body or b"{}")
    assert envelope["command_id"].startswith("cmd_") and envelope["params"] == {
        "workspace_id": "ws_1",
        "content": "hi",
    }
    with pytest.raises(ValueError):
        client.command("NotACommand", {})
    with pytest.raises(ApiError) as refused:
        client.get("audit")
    assert (
        refused.value.code == "SCOPE_FORBIDDEN"
        and refused.value.is_catalogued
        and refused.value.details == {"kind": "rbac"}
    )
    harness = Client(
        "https://q.example",
        Credential(tenant_id="tn_1", user_id="usr_1"),
        transport=transport,
    )
    harness.get("me")
    assert calls[-1][2]["x-quansio-tenant"] == "tn_1"


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


@pytest.fixture(scope="module")
def server() -> Iterator[tuple[str, list[str]]]:
    if not ADMIN_URL:
        pytest.skip(
            "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
        )
    if not SERVER_BIN.is_file() or not SEED_BIN.is_file():
        pytest.skip(
            f"BLOCKED_EXTERNAL: {SERVER_BIN} and {SEED_BIN} are needed; build them with cargo build -p quansio-server --bins"
        )
    database = f"quansio_sdk_py_{os.getpid()}"
    seeded = subprocess.run(
        [
            str(SEED_BIN),
            "create",
            "--database",
            database,
            "--tenant",
            "tn_01J8Z3K6F1N8VQ2X5W9Y0SDKPY",
            "--user",
            "usr_01J8Z3K6F1N8VQ2X5W9Y0SDKPY",
            "--workspace",
            "ws_01J8Z3K6F1N8VQ2X5W9Y0SDKPY",
            "--node",
            "wn_01J8Z3K6F1N8VQ2X5W9Y0SDKPY",
        ],
        env={**os.environ, "QUANSIO_DATABASE_URL": ADMIN_URL},
        capture_output=True,
        text=True,
        check=True,
    )
    database_url = next(
        line.split("=", 1)[1]
        for line in seeded.stdout.splitlines()
        if line.startswith("database_url=")
    )
    port = _free_port()
    origin = f"http://127.0.0.1:{port}"
    env = {
        **os.environ,
        "QUANSIO_DATABASE_URL": database_url,
        "QUANSIO_BIND": f"127.0.0.1:{port}",
        "QUANSIO_APPROVAL_SIGNING_KEY": "sdk-py-e2e-key",
    }
    env.pop("QUANSIO_TRUST_SCOPE_HEADERS", None)
    process = subprocess.Popen(
        [str(SERVER_BIN)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    stderr: list[str] = []

    def pump() -> None:
        assert process.stderr is not None
        stderr.extend(process.stderr)

    threading.Thread(target=pump, daemon=True).start()
    deadline = time.monotonic() + 60
    while True:
        try:
            if Client(origin).health().get("status") == "ready":
                break
        except (OSError, ApiError, ValueError):
            pass
        if process.poll() is not None:
            raise RuntimeError(f"quansio-server exited: {''.join(stderr)}")
        if time.monotonic() > deadline:
            raise RuntimeError(
                f"quansio-server did not become healthy: {''.join(stderr)}"
            )
        time.sleep(0.2)
    try:
        yield origin, stderr
    finally:
        process.terminate()
        process.wait(timeout=10)
        subprocess.run(
            [str(SEED_BIN), "drop", "--database", database],
            env={**os.environ, "QUANSIO_DATABASE_URL": ADMIN_URL},
            capture_output=True,
            check=False,
        )


def test_the_generated_table_is_what_the_current_server_serves(
    server: tuple[str, list[str]],
) -> None:
    origin, stderr = server
    anonymous = Client(origin)
    health = anonymous.health()
    assert health["status"] == "ready"
    groups = anonymous.get("commands")["groups"]
    served = sorted(name for names in groups.values() for name in names)
    assert sorted(OPERATIONS) == served
    assert health["commands"] == len(OPERATIONS)
    assert sorted(anonymous.get("read-projections")["read_projections"]) == sorted(
        READ_PROJECTIONS
    )
    assert health["read_projections"] == len(READ_PROJECTIONS)
    # A session from the magic link on the console, through the SDK.
    email = "sdk-py@example.com"
    anonymous.start_magic_link(email, "SDK")
    deadline = time.monotonic() + 15
    token = None
    while token is None:
        for line in reversed(stderr):
            if f"magic-link for {email}" in line and "token=" in line:
                token = line.split("token=", 1)[1].strip()
                break
        if token is None:
            assert time.monotonic() < deadline, "no magic link on the console"
            time.sleep(0.1)
    session = anonymous.complete_magic_link(token)
    client = Client(origin, Credential(access_token=lambda: session["access_token"]))
    me = client.me()
    assert me["user"]["primary_email"] == email
    assert me["session"]["tenant_id"] == session["tenant_id"]
    posted = client.command(
        "PostMessage",
        {"workspace_id": session["workspace_id"], "content": "hello from python"},
    )
    assert posted["run_id"].startswith("run_")
    assert client.get(f"messages?thread_id={posted['thread_id']}")["messages"]
    with pytest.raises(ApiError) as refused:
        client.command(
            "ApproveEffect",
            {
                "approval_request_id": "apr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
                "params_digest": "0" * 64,
            },
        )
    assert refused.value.is_catalogued
    first = client.command_envelope(
        "CreateWorkspace", {"name": "sdk"}, "cmd_01J8Z3K6F1N8VQ2X5W9Y0SDKPY"
    )
    again = client.command_envelope(
        "CreateWorkspace", {"name": "sdk"}, "cmd_01J8Z3K6F1N8VQ2X5W9Y0SDKPY"
    )
    assert (
        first["replayed"] is False
        and again["replayed"] is True
        and again["result"] == first["result"]
    )
