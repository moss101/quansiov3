"""QA-009 harness: the deployable `quansio-server` under load, measured from the outside.

The server is started as a deployment runs it (DOSSIER §9/§16): the schema applied over the owner's
DSN, requests served over a least-privileged `quansio_app` login, real bearer sessions from the
magic-link flow, the object store and search index on, and every tuning knob at its shipped default
unless a phase says otherwise (and records that it did). Clients are separate threads holding their
own keep-alive connections, the way desktops and SDKs hold them. Timings are wall-clock on the
client, so they include HTTP, the server and PostgreSQL — what a person waits for.

Environment (absent → `BLOCKED_EXTERNAL`):

* `QUANSIO_TEST_POSTGRES_URL` — superuser DSN for scratch databases (the dev stack);
* `QUANSIO_TEST_SERVER_BIN` / `QUANSIO_TEST_SEED_BIN` — the builds under test (a release build
  for SLO numbers; the run records which binary it measured);
* `QUANSIO_TEST_MINIO_*` — dev-stack MinIO, for artifact content.
"""

from __future__ import annotations

import base64
import http.client
import json
import os
import secrets
import shutil
import socket
import statistics
import struct
import subprocess
import tempfile
import threading
import time
from collections.abc import Callable, Iterator, Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import urlencode, urlsplit, urlunsplit

ROOT = Path(__file__).resolve().parents[2]
SERVER_BIN = Path(
    os.environ.get("QUANSIO_TEST_SERVER_BIN", "").strip()
    or ROOT / "target" / "release" / "quansio-server"
)
SEED_BIN = Path(
    os.environ.get("QUANSIO_TEST_SEED_BIN", "").strip()
    or ROOT / "target" / "release" / "quansio-dev-seed"
)
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0Q9PAA"
OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0Q9PAA"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0Q9PAA"
NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0Q9PAA"
#: Fixed rows `quansio-dev-seed --intelligence` writes (see crates/server/src/bin/quansio-dev-seed.rs).
SEEDED_KNOWLEDGE_TERM = "signed"


#: The per-tenant command quota is a policy, not a latency: the latency phases raise it (recorded in
#: each result) so a burst measures the server rather than the quota; the overload phase keeps it.
UNLIMITED = {"QUANSIO_API_RATE_LIMIT": "100000000"}


def record(metric: str, result: dict[str, Any]) -> dict[str, Any]:
    """Print one measurement, and write it to `QUANSIO_PERF_OUT/<metric>.json` when that is set."""
    result = {"metric": metric, "binary": str(SERVER_BIN), **result}
    out = os.environ.get("QUANSIO_PERF_OUT", "").strip()
    if out:
        Path(out).mkdir(parents=True, exist_ok=True)
        (Path(out) / f"{metric}.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"QA-009 {metric}: {json.dumps(result)}")
    return result


def meter(usage: Mapping[str, Any], name: str) -> float:
    """One meter's total from `GET /v1/usage`."""
    return next(
        (float(entry["total"]) for entry in usage["meters"] if entry["meter"] == name),
        0.0,
    )


def blocked_reason() -> str | None:
    if not ADMIN_URL:
        return "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set (start the dev stack: scripts/dev/up)"
    for binary in (SERVER_BIN, SEED_BIN):
        if not binary.is_file():
            return f"BLOCKED_EXTERNAL: {binary} is missing; build it with cargo build --release -p quansio-server --bins"
    return None


def command_id() -> str:
    """A canonical `cmd_` id: 10 characters of time and 16 of randomness, Crockford base32."""
    alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
    stamp = int(time.time() * 1000)
    head = "".join(alphabet[(stamp >> (5 * (9 - index))) & 31] for index in range(10))
    tail = "".join(secrets.choice(alphabet) for _ in range(16))
    return f"cmd_{head}{tail}"


def percentile(samples: Sequence[float], fraction: float) -> float:
    """Nearest-rank percentile (the p95 a dashboard shows), not an interpolation."""
    if not samples:
        raise ValueError("no samples")
    ordered = sorted(samples)
    rank = max(1, int(-(-fraction * len(ordered) // 1)))
    return ordered[rank - 1]


def summary(samples_ms: Sequence[float]) -> dict[str, float | int]:
    return {
        "n": len(samples_ms),
        "p50_ms": round(percentile(samples_ms, 0.50), 2),
        "p95_ms": round(percentile(samples_ms, 0.95), 2),
        "p99_ms": round(percentile(samples_ms, 0.99), 2),
        "max_ms": round(max(samples_ms), 2),
        "mean_ms": round(statistics.fmean(samples_ms), 2),
    }


class ApiFailure(Exception):
    def __init__(self, status: int, body: str) -> None:
        super().__init__(f"HTTP {status}: {body[:300]}")
        self.status = status
        self.body = body

    @property
    def code(self) -> str:
        try:
            return str(json.loads(self.body).get("code", ""))
        except ValueError:
            return ""


class Connection:
    """One client's keep-alive HTTP/1.1 connection (reopened if the server closes it)."""

    def __init__(self, origin: str, token: str | None) -> None:
        parts = urlsplit(origin)
        self.host = parts.hostname or "127.0.0.1"
        self.port = parts.port or 80
        self.token = token
        self._conn: http.client.HTTPConnection | None = None

    def _connection(self) -> http.client.HTTPConnection:
        if self._conn is None:
            self._conn = http.client.HTTPConnection(self.host, self.port, timeout=60)
        return self._conn

    def request(
        self, method: str, path: str, body: Mapping[str, Any] | None = None
    ) -> Any:
        headers = {"accept": "application/json"}
        if self.token:
            headers["authorization"] = f"Bearer {self.token}"
        payload = None
        if body is not None:
            payload = json.dumps(body).encode()
            headers["content-type"] = "application/json"
        for attempt in (0, 1):
            connection = self._connection()
            try:
                connection.request(method, path, body=payload, headers=headers)
                response = connection.getresponse()
                text = response.read().decode("utf-8", errors="replace")
            except (
                http.client.RemoteDisconnected,
                BrokenPipeError,
                ConnectionResetError,
            ):
                # The server closed an idle keep-alive connection: reconnect once (not a retry of
                # a request the server saw — nothing was read back).
                self.close()
                if attempt:
                    raise
                continue
            if response.status >= 400:
                raise ApiFailure(response.status, text)
            return json.loads(text) if text else None
        raise AssertionError("unreachable")

    def get(self, resource: str, **query: str) -> Any:
        path = resource if not query else f"{resource}?{urlencode(query)}"
        return self.request("GET", path)

    def command(self, name: str, params: Mapping[str, Any]) -> Any:
        return self.request(
            "POST",
            f"/v1/commands/{name}",
            {"command_id": command_id(), "params": dict(params)},
        )

    def close(self) -> None:
        if self._conn is not None:
            self._conn.close()
            self._conn = None


class WebSocket:
    """A minimal RFC 6455 client for `/v1/stream` (text frames, ping/pong, close)."""

    def __init__(
        self, origin: str, token: str, subscribe: Mapping[str, Any], read: bool = True
    ) -> None:
        parts = urlsplit(origin)
        self.sock = socket.create_connection((parts.hostname, parts.port), timeout=30)
        if not read:
            # A consumer that never reads: the smallest receive buffer the OS allows, so the
            # server meets backpressure early.
            self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET /v1/stream HTTP/1.1\r\nHost: {parts.hostname}:{parts.port}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
            f"Authorization: Bearer {token}\r\n\r\n"
        )
        self.sock.sendall(request.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(1)
            if not chunk:
                raise ConnectionError("stream handshake closed")
            head += chunk
        status = head.split(b"\r\n", 1)[0]
        if b" 101 " not in status:
            raise ConnectionError(f"stream handshake refused: {status.decode()}")
        self._buffer = b""
        self.send_text(json.dumps(subscribe))

    def send_text(self, text: str) -> None:
        payload = text.encode()
        mask = os.urandom(4)
        header = bytes([0x81])
        if len(payload) < 126:
            header += bytes([0x80 | len(payload)])
        elif len(payload) < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", len(payload))
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", len(payload))
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def _read(self, count: int) -> bytes:
        while len(self._buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("stream closed")
            self._buffer += chunk
        data, self._buffer = self._buffer[:count], self._buffer[count:]
        return data

    def recv(self, timeout: float) -> str | None:
        """The next text message, or `None` when nothing arrives within `timeout` seconds."""
        self.sock.settimeout(timeout)
        message = b""
        try:
            while True:
                first, second = self._read(2)
                opcode = first & 0x0F
                length = second & 0x7F
                if length == 126:
                    length = struct.unpack(">H", self._read(2))[0]
                elif length == 127:
                    length = struct.unpack(">Q", self._read(8))[0]
                payload = self._read(length)
                if opcode == 0x9:  # ping
                    self._pong(payload)
                    continue
                if opcode == 0x8:
                    raise ConnectionError(
                        f"stream closed by server: {payload[2:].decode(errors='replace')}"
                    )
                message += payload
                if first & 0x80:
                    return message.decode("utf-8", errors="replace")
        except TimeoutError:
            return None

    def _pong(self, payload: bytes) -> None:
        mask = os.urandom(4)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.sock.sendall(bytes([0x8A, 0x80 | len(payload)]) + mask + masked)

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def run_concurrently(
    work: Callable[[Connection, int], None],
    connections: Sequence[Connection],
    total: int,
) -> list[BaseException]:
    """Spread `total` calls of `work` across one thread per connection; returns the failures."""
    counter = iter(range(total))
    lock = threading.Lock()
    failures: list[BaseException] = []

    def worker(connection: Connection) -> None:
        while True:
            with lock:
                index = next(counter, None)
            if index is None:
                return
            try:
                work(connection, index)
            except Exception as error:  # noqa: BLE001 - recorded, and the caller asserts on them
                with lock:
                    failures.append(error)

    with ThreadPoolExecutor(max_workers=len(connections)) as pool:
        for future in [pool.submit(worker, connection) for connection in connections]:
            future.result()
    return failures


def rss_mb(pid: int) -> float:
    """Resident memory of a process in MiB (from `ps`, which every supported host has)."""
    out = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True, check=True
    )
    return int(out.stdout.strip()) / 1024


@dataclass
class ServerUnderLoad:
    origin: str
    database: str
    owner_url: str
    serving_role: str
    stderr_path: Path
    process: subprocess.Popen[str]
    settings: dict[str, str]
    token: str = ""
    metrics_token: str = ""
    samples: dict[str, Any] = field(default_factory=dict)

    def log_text(self) -> str:
        return self.stderr_path.read_text(encoding="utf-8", errors="replace")

    def client(self) -> Connection:
        return Connection(self.origin, self.token)

    def sql(
        self, statement: str, params: tuple[Any, ...] = ()
    ) -> list[tuple[Any, ...]]:
        """Read measurement rows as the superuser (row-level security is forced even for the owner)."""
        import psycopg

        parts = urlsplit(ADMIN_URL)
        admin = urlunsplit(
            (parts.scheme, parts.netloc, f"/{self.database}", parts.query, "")
        )
        with psycopg.connect(admin, autocommit=True) as connection:
            cursor = connection.execute(statement, params)
            return list(cursor.fetchall()) if cursor.description else []

    def rss_mb(self) -> float:
        return rss_mb(self.process.pid)

    def sign_in(self, email: str) -> str:
        anonymous = Connection(self.origin, None)
        mark = len(self.log_text())
        anonymous.request(
            "POST",
            "/v1/auth/magic-link/start",
            {"email": email, "display_name": "qa009"},
        )
        deadline = time.monotonic() + 20
        while True:
            for line in reversed(self.log_text()[mark:].splitlines()):
                if (
                    f"magic-link for {email.lower()}" in line.lower()
                    and "token=" in line
                ):
                    token = line.split("token=", 1)[1].strip().strip('"}').split('"')[0]
                    session = anonymous.request(
                        "POST", "/v1/auth/magic-link/complete", {"token": token}
                    )
                    return str(session["access_token"])
            if time.monotonic() > deadline:
                raise RuntimeError(f"no magic link for {email} on the console")
            time.sleep(0.05)


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
            "--intelligence",
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


def _serving_url(owner_url: str, database: str, role: str) -> str:
    import psycopg

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
    return urlunsplit(
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


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


@contextmanager
def start(
    label: str, overrides: Mapping[str, str] | None = None
) -> Iterator[ServerUnderLoad]:
    """Seed a scratch database, start the server as the application role, sign the owner in."""
    database = f"quansio_qa009_{label}_{os.getpid()}"
    role = f"qa009_app_{label}_{os.getpid()}"
    owner_url = _seed(database)
    serving_url = _serving_url(owner_url, database, role)
    workdir = Path(tempfile.mkdtemp(prefix="qa009-"))
    master_key = workdir / "master.key"
    master_key.write_text(base64.b64encode(secrets.token_bytes(32)).decode())
    master_key.chmod(0o600)
    stderr_path = workdir / "server.log"
    port = _free_port()
    metrics_token = f"metrics-{secrets.token_hex(8)}"
    settings = dict(overrides or {})
    env = {
        **os.environ,
        "QUANSIO_DATABASE_URL": serving_url,
        "QUANSIO_MIGRATION_DATABASE_URL": owner_url,
        "QUANSIO_BIND": f"127.0.0.1:{port}",
        "QUANSIO_SECRETS_KEY_FILE": str(master_key),
        "QUANSIO_OBJECT_STORE": "1",
        "QUANSIO_SEARCH_INDEX_ROOT": str(workdir / "index"),
        "QUANSIO_METRICS_TOKEN": metrics_token,
        "QUANSIO_APPROVAL_SIGNING_KEY": f"qa009-{secrets.token_hex(16)}",
        **settings,
    }
    for knob in (
        "QUANSIO_TRUST_SCOPE_HEADERS",
        "QUANSIO_INTELLIGENCE_ENDPOINT",
        "QUANSIO_MAX_CONNECTIONS",
        "QUANSIO_SCHEDULER_INTERVAL_MS",
        "QUANSIO_API_RATE_LIMIT",
        "QUANSIO_SCHEDULER",
    ):
        if knob not in settings:
            env.pop(
                knob, None
            )  # the shipped default, not whatever the calling shell carries
    log = stderr_path.open("w", encoding="utf-8")
    process = subprocess.Popen(
        [str(SERVER_BIN)], env=env, stdout=log, stderr=log, text=True
    )
    server = ServerUnderLoad(
        origin=f"http://127.0.0.1:{port}",
        database=database,
        owner_url=owner_url,
        serving_role=role,
        stderr_path=stderr_path,
        process=process,
        settings=settings,
        metrics_token=metrics_token,
    )
    try:
        deadline = time.monotonic() + 120
        while True:
            try:
                if (
                    Connection(server.origin, None).get("/v1/health").get("status")
                    == "ready"
                ):
                    break
            except (OSError, ApiFailure, ValueError, http.client.HTTPException):
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
        server.token = server.sign_in(f"{OWNER}@example.com")
        yield server
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
        log.close()
        _drop(database, role)
        shutil.rmtree(workdir, ignore_errors=True)


def wait_for(
    predicate: Callable[[], bool],
    what: str,
    timeout: float = 30.0,
    interval: float = 0.05,
) -> float:
    """Poll until `predicate` holds; returns the seconds it took."""
    started = time.monotonic()
    while not predicate():
        if time.monotonic() - started > timeout:
            raise AssertionError(f"timed out waiting for {what}")
        time.sleep(interval)
    return time.monotonic() - started


#: The live provider (an OpenAI-compatible endpoint), from `QUANSIO_TEST_OPENAI_COMPATIBLE_*`.
LIVE_BASE_URL = (
    os.environ.get("QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL", "").strip().rstrip("/")
)
LIVE_API_KEY = os.environ.get("QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY", "").strip()
LIVE_MODEL_ID = os.environ.get("QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID", "").strip()


def live_blocked_reason() -> str | None:
    blocked = blocked_reason()
    if blocked:
        return blocked
    if not (LIVE_BASE_URL and LIVE_API_KEY and LIVE_MODEL_ID):
        return (
            "BLOCKED_EXTERNAL: QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL / _API_KEY / _MODEL_ID are not set "
            "(a live provider for model-stream and usage measurements)"
        )
    return None


#: The dev stack's deterministic OpenAI-compatible stub provider: it answers at once, so the time a
#: first token takes through Quansio over the stub's own is Quansio's own overhead.
STUB_PROVIDER_URL = (
    os.environ.get("QUANSIO_TEST_STUB_PROVIDER_URL", "").strip() or "http://127.0.0.1:59020"
).rstrip("/")


def stub_blocked_reason() -> str | None:
    blocked = blocked_reason()
    if blocked:
        return blocked
    parts = urlsplit(STUB_PROVIDER_URL)
    try:
        with socket.create_connection((parts.hostname, parts.port or 80), timeout=2):
            return None
    except OSError:
        return f"BLOCKED_EXTERNAL: the dev stub provider at {STUB_PROVIDER_URL} is not reachable (scripts/dev/up)"


@contextmanager
def gateway(
    base_url: str = LIVE_BASE_URL, model_id: str = LIVE_MODEL_ID, api_key: str = LIVE_API_KEY
) -> Iterator[str]:
    """The Python intelligence service (`python -m intelligence.server`) routed to one provider
    (the live one by default).

    The credential reaches the process only through its environment, under the handle the catalog
    names; it is never written to a file or a log.
    """
    workdir = Path(tempfile.mkdtemp(prefix="qa009-gateway-"))
    catalog = workdir / "models.yaml"
    catalog.write_text(
        "version: 1\n"
        "providers:\n  openai-compatible:\n    kind: openai_compatible\n"
        f"    base_url: {base_url}\n    credential_handle: provider/qa009-live\n"
        "routing:\n  strategy: deterministic\n  primary: qa009-live\n  fallback_order: []\n"
        "models:\n  - id: qa009-live\n    provider: openai-compatible\n"
        f"    model_id: {model_id}\n    capabilities: [tools, streaming]\n"
        "    context_window: 128000\n    cost_class: low\n    dlp_eligible: false\n",
        encoding="utf-8",
    )
    port = _free_port()
    log = (workdir / "gateway.log").open("w", encoding="utf-8")
    process = subprocess.Popen(
        [
            "uv",
            "run",
            "--project",
            str(ROOT / "python"),
            "python",
            "-m",
            "intelligence.server",
            "--transport",
            "tcp",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
        ],
        env={
            **os.environ,
            "QUANSIO_INTELLIGENCE_MODEL_CATALOG": str(catalog),
            "QUANSIO_SECRET_PROVIDER_QA009_LIVE": api_key,
        },
        stdout=log,
        stderr=log,
        text=True,
    )
    try:
        deadline = time.monotonic() + 120
        while True:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=1):
                    break
            except OSError:
                if process.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(
                        "the intelligence service did not start"
                    ) from None
                time.sleep(0.25)
        yield f"http://127.0.0.1:{port}"
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
        log.close()
        shutil.rmtree(workdir, ignore_errors=True)


def provider_first_token_ms(
    prompt: str,
    max_tokens: int = 4096,
    base_url: str = LIVE_BASE_URL,
    model_id: str = LIVE_MODEL_ID,
    api_key: str = LIVE_API_KEY,
) -> float:
    """Time to the first content delta straight from the provider (the §21.2 baseline).

    A reasoning model streams its reasoning before its answer; the first *content* token is what a
    person sees, through Quansio or directly, so that is what both sides time. `max_tokens` matches
    `config/intelligence.yaml` (`max_output_tokens`), so both calls may reason as far.
    """
    import ssl
    import urllib.request

    body = json.dumps(
        {
            "model": model_id,
            "stream": True,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": prompt}],
        }
    ).encode()
    request = urllib.request.Request(
        f"{base_url}/chat/completions",
        data=body,
        headers={
            "authorization": f"Bearer {api_key}",
            "content-type": "application/json",
            "accept": "text/event-stream",
        },
        method="POST",
    )
    started = time.perf_counter()
    with urllib.request.urlopen(
        request, timeout=120, context=ssl.create_default_context()
    ) as response:
        for raw in response:
            line = raw.decode("utf-8", errors="replace").strip()
            if not line.startswith("data:") or line == "data: [DONE]":
                continue
            chunk = json.loads(line[5:])
            for choice in chunk.get("choices") or []:
                if (choice.get("delta") or {}).get("content"):
                    return (time.perf_counter() - started) * 1000
    raise AssertionError("the provider streamed no content")
