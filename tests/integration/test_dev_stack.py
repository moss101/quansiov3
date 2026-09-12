"""Real-stack smoke and determinism tests for the local dev environment (GOV-007).

These tests execute the committed `scripts/dev/*` commands against a real Docker
daemon and assert observable state (Postgres rows, the pgvector extension, NATS
JetStream, MinIO bucket). They skip with an explicit ``BLOCKED_EXTERNAL`` reason
only when Docker itself is unavailable.

Connection settings come from environment variables; each one falls back to the
documented dev default from `config/dev.yaml`:

  QUANSIO_TEST_COMPOSE_PROJECT        compose project name            (quansio-dev-test)
  QUANSIO_TEST_COMPOSE_FILE           compose file path               (infra/compose/compose.yaml)
  QUANSIO_TEST_POSTGRES_HOST          dev Postgres host               (127.0.0.1)
  QUANSIO_TEST_POSTGRES_PORT          dev Postgres port               (56440)
  QUANSIO_TEST_POSTGRES_USER          dev database user               (quansio)
  QUANSIO_TEST_POSTGRES_PASSWORD      dev-only password               (quansio-dev-only)
  QUANSIO_TEST_POSTGRES_DB            dev database name               (quansio)
  QUANSIO_TEST_NATS_HOST              NATS host                       (127.0.0.1)
  QUANSIO_TEST_NATS_PORT              NATS client port                (55230)
  QUANSIO_TEST_NATS_MONITOR_PORT      NATS monitoring port            (55231)
  QUANSIO_TEST_MINIO_HOST             MinIO S3 API host               (127.0.0.1)
  QUANSIO_TEST_MINIO_PORT             MinIO S3 API port               (59110)
  QUANSIO_TEST_MINIO_ROOT_USER        MinIO root user                 (quansio-dev)
  QUANSIO_TEST_MINIO_ROOT_PASSWORD    dev-only MinIO password         (quansio-dev-only)
  QUANSIO_TEST_MINIO_BUCKET           dev object-storage bucket       (quansio-dev)
  QUANSIO_TEST_STUB_PROVIDER_HOST     stub model provider host        (127.0.0.1)
  QUANSIO_TEST_STUB_PROVIDER_PORT     stub model provider port        (59120)
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import urllib.request
from pathlib import Path

import psycopg
import pytest

ROOT = Path(__file__).resolve().parents[2]
COMPOSE_FILE = ROOT / os.environ.get("QUANSIO_TEST_COMPOSE_FILE", "infra/compose/compose.yaml")
ENV_FILE = ROOT / ".env"

# The suite exercises its OWN compose project on offset ports by default: it runs
# `scripts/dev/reset`, which tears the stack down with its volumes, and the default
# project/ports may be in use by a developer or another agent. Override the
# QUANSIO_TEST_* values to point the suite at a different stack.
COMPOSE_PROJECT = os.environ.get("QUANSIO_TEST_COMPOSE_PROJECT", "quansio-dev-test")
POSTGRES_HOST = os.environ.get("QUANSIO_TEST_POSTGRES_HOST", "127.0.0.1")
POSTGRES_PORT = os.environ.get("QUANSIO_TEST_POSTGRES_PORT", "56440")
POSTGRES_USER = os.environ.get("QUANSIO_TEST_POSTGRES_USER", "quansio")
POSTGRES_PASSWORD = os.environ.get("QUANSIO_TEST_POSTGRES_PASSWORD", "quansio-dev-only")
POSTGRES_DB = os.environ.get("QUANSIO_TEST_POSTGRES_DB", "quansio")
NATS_HOST = os.environ.get("QUANSIO_TEST_NATS_HOST", "127.0.0.1")
NATS_PORT = os.environ.get("QUANSIO_TEST_NATS_PORT", "55230")
NATS_MONITOR_PORT = os.environ.get("QUANSIO_TEST_NATS_MONITOR_PORT", "55231")
MINIO_HOST = os.environ.get("QUANSIO_TEST_MINIO_HOST", "127.0.0.1")
MINIO_PORT = os.environ.get("QUANSIO_TEST_MINIO_PORT", "59110")
MINIO_ROOT_USER = os.environ.get("QUANSIO_TEST_MINIO_ROOT_USER", "quansio-dev")
MINIO_ROOT_PASSWORD = os.environ.get("QUANSIO_TEST_MINIO_ROOT_PASSWORD", "quansio-dev-only")
MINIO_BUCKET = os.environ.get("QUANSIO_TEST_MINIO_BUCKET", "quansio-dev")
STUB_PROVIDER_HOST = os.environ.get("QUANSIO_TEST_STUB_PROVIDER_HOST", "127.0.0.1")
STUB_PROVIDER_PORT = os.environ.get("QUANSIO_TEST_STUB_PROVIDER_PORT", "59120")

TENANT_ID = "tn_01J8Z3K6F1DEV0000000000TEN"
WORKSPACE_ID = "ws_01J8Z3K6F1DEV0000000000WKS"
TEAMMATE_ID = "agt_01J8Z3K6F1DEV0000000000AGT"
EXPECTED_IDS = {
    "tenant": TENANT_ID,
    "workspace": WORKSPACE_ID,
    "teammate": TEAMMATE_ID,
}


def _docker_available() -> bool:
    if shutil.which("docker") is None:
        return False
    try:
        probe = subprocess.run(["docker", "info"], capture_output=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return probe.returncode == 0


@pytest.fixture(scope="module", autouse=True)
def require_docker() -> None:
    if not _docker_available():
        pytest.skip(
            "BLOCKED_EXTERNAL: Docker daemon unavailable; cannot exercise the dev stack"
        )


# Port and project overrides that keep this suite's stack separate from any other one.
ISOLATION = {
    "QUANSIO_DEV_COMPOSE_PROJECT": COMPOSE_PROJECT,
    "QUANSIO_DEV_POSTGRES_PORT": POSTGRES_PORT,
    "QUANSIO_DEV_NATS_PORT": NATS_PORT,
    "QUANSIO_DEV_NATS_MONITOR_PORT": NATS_MONITOR_PORT,
    "QUANSIO_DEV_MINIO_PORT": MINIO_PORT,
    "QUANSIO_DEV_MINIO_CONSOLE_PORT": str(int(MINIO_PORT) + 1),
    "QUANSIO_DEV_STUB_PROVIDER_PORT": STUB_PROVIDER_PORT,
}


def run_dev(command: str, *args: str, timeout: int = 300) -> subprocess.CompletedProcess[str]:
    script = ROOT / "scripts" / "dev" / command
    assert script.is_file(), f"missing committed script: {script}"
    env = {**os.environ, **ISOLATION}
    return subprocess.run(
        ["bash", str(script), *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
    )


def _dsn() -> str:
    return (
        f"host={POSTGRES_HOST} port={POSTGRES_PORT} user={POSTGRES_USER} "
        f"password={POSTGRES_PASSWORD} dbname={POSTGRES_DB} connect_timeout=5"
    )


def query(sql: str) -> list[tuple[object, ...]]:
    with psycopg.connect(_dsn(), autocommit=True) as conn, conn.cursor() as cur:
        cur.execute(sql)
        return cur.fetchall()


def seeded_identifiers() -> dict[str, str]:
    return {kind: ident for kind, ident in query("SELECT kind, id FROM dev.seed_identity")}


def _minio_bucket_present() -> bool:
    container = subprocess.run(
        [
            "docker", "compose", "-p", COMPOSE_PROJECT, "-f", str(COMPOSE_FILE),
            "--env-file", str(ENV_FILE), "ps", "-q", "minio",
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )
    container_id = container.stdout.strip().splitlines()[0] if container.stdout.strip() else ""
    if not container_id:
        return False
    # Inside the container the S3 API listens on 9000; MINIO_HOST/MINIO_PORT are
    # the host-side mapping used by the health probe above.
    listing = subprocess.run(
        [
            "docker", "exec", "-e",
            f"MC_HOST_dev=http://{MINIO_ROOT_USER}:{MINIO_ROOT_PASSWORD}@127.0.0.1:9000",
            container_id, "mc", "ls", f"dev/{MINIO_BUCKET}",
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )
    return listing.returncode == 0


def _http_status(url: str) -> int:
    with urllib.request.urlopen(url, timeout=5) as response:
        return int(response.status)


def _stub_completion() -> str:
    payload = json.dumps(
        {"model": "local-instruct", "messages": [{"role": "user", "content": "hello dev"}]}
    ).encode()
    request = urllib.request.Request(
        f"http://{STUB_PROVIDER_HOST}:{STUB_PROVIDER_PORT}/v1/chat/completions",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status == 200
        return response.read().decode()


def test_bootstrap_smoke() -> None:
    up = run_dev("up")
    assert up.returncode == 0, f"scripts/dev/up failed:\n{up.stdout}\n{up.stderr}"

    health = run_dev("healthcheck")
    assert health.returncode == 0, f"scripts/dev/healthcheck failed:\n{health.stdout}\n{health.stderr}"

    seed = run_dev("seed")
    assert seed.returncode == 0, f"scripts/dev/seed failed:\n{seed.stdout}\n{seed.stderr}"

    extensions = {row[0] for row in query("SELECT extname FROM pg_extension")}
    assert "vector" in extensions, f"pgvector extension missing; extensions={sorted(extensions)}"

    assert seeded_identifiers() == EXPECTED_IDS

    tenant_id, workspace_id = query(
        "SELECT tenant_id, workspace_id FROM dev.seed_identity WHERE kind = 'teammate'"
    )[0]
    assert tenant_id == TENANT_ID
    assert workspace_id == WORKSPACE_ID

    with socket.create_connection((NATS_HOST, int(NATS_PORT)), timeout=5):
        pass
    assert _http_status(f"http://{NATS_HOST}:{NATS_MONITOR_PORT}/healthz") == 200
    with urllib.request.urlopen(f"http://{NATS_HOST}:{NATS_MONITOR_PORT}/jsz", timeout=5) as response:
        jetstream = json.load(response)
    assert isinstance(jetstream.get("config"), dict), "JetStream is not enabled on the dev NATS server"

    assert _http_status(f"http://{MINIO_HOST}:{MINIO_PORT}/minio/health/ready") == 200
    assert _minio_bucket_present(), f"dev bucket {MINIO_BUCKET!r} is missing from MinIO"

    first_completion = _stub_completion()
    assert '"stub completion' in first_completion
    assert _stub_completion() == first_completion, "stub model provider is not deterministic"


def test_reset_then_reseed_is_deterministic() -> None:
    up = run_dev("up")
    assert up.returncode == 0, f"scripts/dev/up failed:\n{up.stdout}\n{up.stderr}"
    seed = run_dev("seed")
    assert seed.returncode == 0, f"scripts/dev/seed failed:\n{seed.stdout}\n{seed.stderr}"
    before_reset = seeded_identifiers()
    assert before_reset == EXPECTED_IDS

    reset = run_dev("reset", timeout=600)
    assert reset.returncode == 0, f"scripts/dev/reset failed:\n{reset.stdout}\n{reset.stderr}"

    table_exists = query("SELECT to_regclass('dev.seed_identity') IS NOT NULL")[0][0]
    if table_exists:
        assert query("SELECT count(*) FROM dev.seed_identity")[0][0] == 0, "reset left seeded rows behind"

    for _ in range(2):
        seed = run_dev("seed")
        assert seed.returncode == 0, f"scripts/dev/seed failed:\n{seed.stdout}\n{seed.stderr}"
        assert seeded_identifiers() == EXPECTED_IDS
    assert seeded_identifiers() == before_reset
