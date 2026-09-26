"""QA-007 harness: the real `quansio-server` under attack, serving as the least-privileged role.

The deployable is started the way DOSSIER §9/§16 intends a deployment to run it: the schema is applied
over the owner's DSN (`QUANSIO_MIGRATION_DATABASE_URL`) and requests are served over a login role that
is a member of `quansio_app` — never the table owner, never a superuser, so forced row-level security
applies to every statement the server makes. Sessions are real bearer sessions from the magic-link flow
(`QUANSIO_TRUST_SCOPE_HEADERS` is removed, so forged scope headers must be ignored).

Identities:

* `owner`, `approver`, `viewer` — the seeded tenant A (owner; `member`/`approver`; `member`/`viewer`);
* `outsider` — a fresh address that onboards into its own tenant B.

A secret canary is planted where secrets live (the provider credential variable, the approval signing
key, the secrets master key) and every read surface and the server's own log are scanned for it.

Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN for scratch databases),
`QUANSIO_TEST_SERVER_BIN` / `QUANSIO_TEST_SEED_BIN` (default `target/debug`), `QUANSIO_TEST_MINIO_*`
(dev-stack MinIO, for artifact content). Absent → `BLOCKED_EXTERNAL`.
"""

from __future__ import annotations

import base64
import http.client
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from collections.abc import Iterator, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "sdk" / "python"))

from quansio_sdk import ApiError, Client, Credential, new_command_id

SERVER_BIN = Path(
    os.environ.get("QUANSIO_TEST_SERVER_BIN")
    or ROOT / "target" / "debug" / "quansio-server"
)
SEED_BIN = Path(
    os.environ.get("QUANSIO_TEST_SEED_BIN")
    or ROOT / "target" / "debug" / "quansio-dev-seed"
)
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA7AA"
OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA7AA"
APPROVER = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA7AP"
VIEWER = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA7VW"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA7AA"
NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA7AA"
OUTSIDER_EMAIL = "qa007-outsider@example.com"

#: The canary: a value shaped like a secret that must never leave custody.
CANARY = "qncy_test_canary_not_for_prod"
#: A file beside the web root (never inside it) that path traversal must not reach.
OUTSIDE_SECRET_NAME = "outside-secret.txt"
OUTSIDE_SECRET = "qa007-outside-the-web-root-must-never-be-served"


def blocked_reason() -> str | None:
    if not ADMIN_URL:
        return "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
    if not SERVER_BIN.is_file() or not SEED_BIN.is_file():
        return (
            f"BLOCKED_EXTERNAL: {SERVER_BIN} and {SEED_BIN} are needed; "
            "build them with cargo build -p quansio-server --bins"
        )
    return None


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


@dataclass(frozen=True)
class Response:
    status: int
    headers: Mapping[str, str]
    body: bytes

    def json(self) -> Any:
        return json.loads(self.body.decode("utf-8")) if self.body else None

    @property
    def code(self) -> str:
        try:
            decoded = self.json()
        except ValueError:
            return ""
        return str(decoded.get("code", "")) if isinstance(decoded, dict) else ""


@dataclass
class Identity:
    """One signed-in principal: its session, its tenant and an SDK client over its bearer token."""

    name: str
    email: str
    session: dict[str, Any]
    origin: str

    @property
    def token(self) -> str:
        return str(self.session["access_token"])

    @property
    def tenant_id(self) -> str:
        return str(self.session["tenant_id"])

    @property
    def workspace_id(self) -> str:
        return str(self.session["workspace_id"])

    @property
    def client(self) -> Client:
        return Client(self.origin, Credential(access_token=lambda: self.token))

    def command(self, name: str, params: Mapping[str, Any]) -> Any:
        return self.client.command(name, dict(params))

    def refused(self, name: str, params: Mapping[str, Any]) -> ApiError:
        """Run a command that must be refused; the refusal (never a success) is returned."""
        try:
            result = self.client.command(name, dict(params))
        except ApiError as error:
            return error
        raise AssertionError(
            f"{self.name}: {name} was accepted: {json.dumps(result)[:300]}"
        )

    def get(self, resource: str) -> Any:
        return self.client.get(resource)

    def raw(
        self,
        method: str,
        path: str,
        *,
        body: Any = None,
        headers: Mapping[str, str] | None = None,
    ) -> Response:
        return raw_request(
            self.origin,
            method,
            path,
            body=body,
            headers={"authorization": f"Bearer {self.token}", **(headers or {})},
        )


def raw_request(
    origin: str,
    method: str,
    path: str,
    *,
    body: Any = None,
    headers: Mapping[str, str] | None = None,
) -> Response:
    """One HTTP request with the path sent exactly as given (no client-side normalization)."""
    parts = urlsplit(origin)
    connection = http.client.HTTPConnection(
        parts.hostname or "127.0.0.1", parts.port, timeout=30
    )
    payload = (
        None
        if body is None
        else (body if isinstance(body, bytes) else json.dumps(body).encode("utf-8"))
    )
    sent = dict(headers or {})
    if payload is not None:
        sent.setdefault("content-type", "application/json")
    try:
        connection.request(method, path, body=payload, headers=sent)
        answer = connection.getresponse()
        return Response(
            status=answer.status,
            headers={k.lower(): v for k, v in answer.getheaders()},
            body=answer.read(),
        )
    finally:
        connection.close()


@dataclass
class ServerUnderAttack:
    """A scratch database, a least-privileged serving role and the running server."""

    origin: str
    database: str
    owner_url: str
    serving_role: str
    stderr_path: Path
    process: subprocess.Popen[str]
    identities: dict[str, Identity] = field(default_factory=dict)
    issued_secrets: dict[str, str] = field(default_factory=dict)

    def log_text(self) -> str:
        return self.stderr_path.read_text(encoding="utf-8", errors="replace")

    def sign_in(self, name: str, email: str) -> Identity:
        anonymous = Client(self.origin)
        mark = len(self.log_text())
        anonymous.start_magic_link(email, name)
        token = None
        deadline = time.monotonic() + 20
        while token is None:
            for line in reversed(self.log_text()[mark:].splitlines()):
                if (
                    f"magic-link for {email.lower()}" in line.lower()
                    and "token=" in line
                ):
                    token = line.split("token=", 1)[1].strip().strip('"}').split('"')[0]
                    break
            if token is None:
                if time.monotonic() > deadline:
                    raise RuntimeError(f"no magic link for {email} on the console")
                time.sleep(0.1)
        session = anonymous.complete_magic_link(token)
        identity = Identity(name=name, email=email, session=session, origin=self.origin)
        self.identities[name] = identity
        self.issued_secrets[f"access_token:{name}"] = identity.token
        if session.get("refresh_token"):
            self.issued_secrets[f"refresh_token:{name}"] = str(session["refresh_token"])
        return identity

    def sql(
        self, statement: str, params: tuple[Any, ...] = (), *, as_owner: bool = True
    ) -> list[tuple[Any, ...]]:
        import psycopg

        with psycopg.connect(self.owner_url, autocommit=True) as connection:
            cursor = connection.execute(statement, params)
            return list(cursor.fetchall()) if cursor.description else []

    def serving_roles(self) -> set[str]:
        rows = self.sql(
            # Backends still authenticating have no user yet; they are not serving connections.
            "SELECT DISTINCT usename FROM pg_stat_activity "
            "WHERE datname = %s AND pid <> pg_backend_pid() AND usename IS NOT NULL",
            (self.database,),
        )
        return {str(row[0]) for row in rows}


def _seed(database: str) -> str:
    seeded = subprocess.run(
        [
            str(SEED_BIN),
            "create",
            "--database",
            database,
            "--tenant",
            TENANT,
            "--user",
            OWNER,
            "--workspace",
            WORKSPACE,
            "--node",
            NODE,
            "--ask-writes",
            "--member",
            f"{APPROVER}:member:approver",
            "--member",
            f"{VIEWER}:member:viewer",
        ],
        env={**os.environ, "QUANSIO_DATABASE_URL": ADMIN_URL},
        capture_output=True,
        text=True,
        check=True,
    )
    return next(
        line.split("=", 1)[1]
        for line in seeded.stdout.splitlines()
        if line.startswith("database_url=")
    )


def _serving_url(owner_url: str, database: str) -> tuple[str, str]:
    """A fresh login role in `quansio_app` (no superuser, no owner, no BYPASSRLS) and its DSN."""
    import psycopg

    role = f"qa007_app_{os.getpid()}"
    password = secrets.token_hex(16)
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        exists = admin.execute(
            "SELECT 1 FROM pg_roles WHERE rolname = %s", (role,)
        ).fetchone()
        verb = "ALTER" if exists else "CREATE"
        admin.execute(
            f"{verb} ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS PASSWORD '{password}'"
        )
        admin.execute(f"GRANT quansio_app TO {role}")
        admin.execute(f"GRANT CONNECT ON DATABASE {database} TO {role}")
    parts = urlsplit(owner_url)
    return role, urlunsplit(
        (
            parts.scheme,
            f"{role}:{password}@{parts.hostname}:{parts.port}",
            parts.path,
            parts.query,
            "",
        )
    )


def _drop(database: str, role: str) -> None:
    import psycopg

    subprocess.run(
        [str(SEED_BIN), "drop", "--database", database],
        env={**os.environ, "QUANSIO_DATABASE_URL": ADMIN_URL},
        capture_output=True,
        check=False,
    )
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP ROLE IF EXISTS {role}")


def start(label: str = "campaign") -> Iterator[ServerUnderAttack]:
    """Seed, start the server as the application role, sign everyone in; yields, then tears down."""
    database = f"quansio_qa007_{label}_{os.getpid()}"
    owner_url = _seed(database)
    role, serving_url = _serving_url(owner_url, database)
    workdir = Path(tempfile.mkdtemp(prefix="qa007-"))
    master_key = workdir / "master.key"
    master_key.write_text(base64.b64encode(secrets.token_bytes(32)).decode())
    master_key.chmod(0o600)
    stderr_path = workdir / "server.log"
    # A web root to traverse out of, and a file just outside it that must never be served.
    web_root = workdir / "web"
    web_root.mkdir()
    (web_root / "index.html").write_text(
        "<!doctype html><title>qa007</title>", encoding="utf-8"
    )
    (workdir / OUTSIDE_SECRET_NAME).write_text(OUTSIDE_SECRET, encoding="utf-8")
    port = _free_port()
    env = {
        **os.environ,
        "QUANSIO_DATABASE_URL": serving_url,
        "QUANSIO_MIGRATION_DATABASE_URL": owner_url,
        "QUANSIO_BIND": f"127.0.0.1:{port}",
        "QUANSIO_APPROVAL_SIGNING_KEY": f"qa007-signing-{CANARY}",
        "QUANSIO_SECRETS_KEY_FILE": str(master_key),
        "QUANSIO_TEST_SECRET_CANARY": CANARY,
        "QUANSIO_OBJECT_STORE": "1",
        "QUANSIO_METRICS_TOKEN": f"metrics-{secrets.token_hex(8)}",
        "QUANSIO_SCHEDULER_INTERVAL_MS": "200",
        "QUANSIO_WEB_ROOT": str(web_root),
        # A provider credential in the process environment: it must never surface anywhere.
        "QUANSIO_SECRET_PROVIDER_ANTHROPIC": CANARY,
    }
    env.pop("QUANSIO_TRUST_SCOPE_HEADERS", None)
    env.pop("QUANSIO_INTELLIGENCE_ENDPOINT", None)
    log = stderr_path.open("w", encoding="utf-8")
    process = subprocess.Popen(
        [str(SERVER_BIN)], env=env, stdout=log, stderr=log, text=True
    )
    origin = f"http://127.0.0.1:{port}"
    server = ServerUnderAttack(
        origin=origin,
        database=database,
        owner_url=owner_url,
        serving_role=role,
        stderr_path=stderr_path,
        process=process,
    )
    server.issued_secrets["metrics_token"] = env["QUANSIO_METRICS_TOKEN"]
    server.issued_secrets["serving_password"] = urlsplit(serving_url).password or ""
    server.issued_secrets["master_key"] = master_key.read_text()
    try:
        deadline = time.monotonic() + 90
        while True:
            try:
                if Client(origin).health().get("status") == "ready":
                    break
            except (OSError, ApiError, ValueError):
                pass
            if process.poll() is not None:
                raise RuntimeError(
                    f"quansio-server exited: {server.log_text()[-2000:]}"
                )
            if time.monotonic() > deadline:
                raise RuntimeError(
                    f"quansio-server did not become healthy: {server.log_text()[-2000:]}"
                )
            time.sleep(0.2)
        server.sign_in("owner", f"{OWNER}@example.com")
        server.sign_in("approver", f"{APPROVER}@example.com")
        server.sign_in("viewer", f"{VIEWER}@example.com")
        server.sign_in("outsider", OUTSIDER_EMAIL)
        yield server
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
        log.close()
        out = os.environ.get("QUANSIO_SECURITY_OUT", "").strip()
        if out:
            Path(out).mkdir(parents=True, exist_ok=True)
            (Path(out) / f"server-{label}.log").write_text(
                server.log_text(), encoding="utf-8"
            )
        _drop(database, role)
        shutil.rmtree(workdir, ignore_errors=True)


def wait_for(
    predicate: Any,
    *,
    timeout: float = 30.0,
    interval: float = 0.2,
    what: str = "condition",
) -> Any:
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() > deadline:
            raise AssertionError(f"timed out waiting for {what}")
        time.sleep(interval)


def command_id() -> str:
    return new_command_id()


def pump(stream: Any, sink: list[str]) -> threading.Thread:
    thread = threading.Thread(target=lambda: sink.extend(stream), daemon=True)
    thread.start()
    return thread


__all__ = [
    "APPROVER",
    "CANARY",
    "NODE",
    "OUTSIDE_SECRET",
    "OWNER",
    "TENANT",
    "VIEWER",
    "WORKSPACE",
    "ApiError",
    "Identity",
    "Response",
    "ServerUnderAttack",
    "blocked_reason",
    "command_id",
    "raw_request",
    "start",
    "wait_for",
]
