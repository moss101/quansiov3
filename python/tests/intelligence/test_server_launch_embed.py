"""The shipped gateway process, launched twice, serving `Embed` (INT-011).

This drives the real entry point — `python -m intelligence.server` as a subprocess, over real
loopback TCP — rather than constructing the servicer in-process, so it proves what an operator
actually runs: the process composes its own gateway from the catalog, resolves the embedding
route, and answers with vectors. The catalog is pointed at the loopback conformance stub by the
operator override, which is how a deployment configures its providers.

The launch is repeated and both runs must return the same correct vectors: a single successful
start is not evidence that the process serves `Embed` reliably.
"""

from __future__ import annotations

import contextlib
import json
import os
import socket
import subprocess
import sys
import time
from collections.abc import Iterator
from pathlib import Path

import grpc
import pytest
import yaml
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.embeddings import INDEX_DIMENSIONS
from intelligence.model_gateway.conformance import (
    CONFORMANCE_CREDENTIAL_HANDLE,
    CONFORMANCE_CREDENTIAL_VALUE,
    StubProvider,
)
from intelligence.model_gateway.credentials import env_var_for_handle

PY_ROOT = Path(__file__).resolve().parents[2]
REPO_ROOT = PY_ROOT.parent

TENANT_ID = "tn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"
WORKSPACE_ID = "ws_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"
CORRELATION_ID = "corr_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"
CAPABILITY_PROJECTION_ID = "cp_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"

SOURCES = (
    "Deployment runbook. Retention is ninety days for logs.",
    "Memory entries are candidates until a person confirms them.",
)

LAUNCH_ATTEMPTS = 2


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def _stub_catalog(path: Path, base_url: str) -> Path:
    """A copy of the repository catalog with every provider pointed at the loopback stub."""
    data = yaml.safe_load((REPO_ROOT / "config" / "models.yaml").read_text(encoding="utf-8"))
    for entry in data["providers"].values():
        entry.pop("base_url_env", None)
        entry.pop("credential_handle", None)
        entry.pop("credential_handle_env", None)
        entry["base_url"] = base_url
        entry["credential_handle"] = CONFORMANCE_CREDENTIAL_HANDLE
    path.write_text(yaml.safe_dump(data), encoding="utf-8")
    return path


def _scope() -> service_pb2.ScopeContext:
    return service_pb2.ScopeContext(
        schema_version="v1",
        tenant_id=TENANT_ID,
        workspace_id=WORKSPACE_ID,
        correlation_id=CORRELATION_ID,
        deadline_ms=int(time.time() * 1000) + 30_000,
        capability_projection_id=CAPABILITY_PROJECTION_ID,
    )


def _await_port(port: int, process: subprocess.Popen[bytes], timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError(f"the gateway exited with {process.returncode} before binding port {port}")
        with socket.socket() as probe:
            probe.settimeout(0.25)
            if probe.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.1)
    raise AssertionError(f"the gateway did not bind port {port} within {timeout}s")


@contextlib.contextmanager
def launch(
    tmp_path: Path, stub: StubProvider, port: int
) -> Iterator[service_pb2_grpc.IntelligenceGatewayStub]:
    """Start `python -m intelligence.server` for real and yield a client for it."""
    catalog = _stub_catalog(tmp_path / f"models-{port}.yaml", stub.base_url)
    env = {
        **os.environ,
        "QUANSIO_INTELLIGENCE_MODEL_CATALOG": str(catalog),
        "QUANSIO_SECRET_CONFORMANCE_STUB": CONFORMANCE_CREDENTIAL_VALUE,
        env_var_for_handle(CONFORMANCE_CREDENTIAL_HANDLE): CONFORMANCE_CREDENTIAL_VALUE,
    }
    process = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "intelligence.server",
            "--transport",
            "tcp",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
        ],
        cwd=str(PY_ROOT),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    try:
        _await_port(port, process)
        channel = grpc.insecure_channel(f"127.0.0.1:{port}")
        try:
            grpc.channel_ready_future(channel).result(timeout=15.0)
            yield service_pb2_grpc.IntelligenceGatewayStub(channel)
        finally:
            channel.close()
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:  # pragma: no cover - a hung launcher
                process.kill()
                process.wait(timeout=5)
        output = process.stdout.read().decode(errors="replace") if process.stdout else ""
        process.stdout.close() if process.stdout else None
        if process.returncode not in (0, -15) and "Traceback" in output:
            raise AssertionError(f"the gateway launcher failed:\n{output[-2000:]}")


@pytest.fixture(scope="module")
def stub() -> Iterator[StubProvider]:
    with StubProvider() as provider:
        yield provider


def test_the_launched_process_serves_embed_on_every_start(stub: StubProvider, tmp_path: Path) -> None:
    vectors_by_run: list[list[list[float]]] = []
    model_ids: list[str] = []
    for attempt in range(LAUNCH_ATTEMPTS):
        port = _free_port()
        with launch(tmp_path, stub, port) as client:
            response = client.Embed(
                service_pb2.EmbedRequest(schema_version="v1", scope=_scope(), texts=list(SOURCES))
            )
        assert response.dimensions == INDEX_DIMENSIONS, f"launch {attempt + 1}"
        assert len(response.embeddings) == len(SOURCES), f"launch {attempt + 1}"
        assert [embedding.index for embedding in response.embeddings] == [0, 1]
        for embedding in response.embeddings:
            assert len(embedding.vector) == INDEX_DIMENSIONS
            assert any(component != 0.0 for component in embedding.vector), (
                "the launched process must return real vectors, not placeholders"
            )
        vectors_by_run.append([list(embedding.vector) for embedding in response.embeddings])
        model_ids.append(response.model_id)

    assert len(vectors_by_run) == LAUNCH_ATTEMPTS
    assert vectors_by_run[0] == vectors_by_run[1], "every launch must return the same vectors"
    assert len(set(model_ids)) == 1 and model_ids[0].startswith("openai-text-embedding")
    assert vectors_by_run[0][0] != vectors_by_run[0][1], "different inputs must not produce the same vector"


def test_the_launched_process_refuses_an_empty_embed_request(stub: StubProvider, tmp_path: Path) -> None:
    port = _free_port()
    with launch(tmp_path, stub, port) as client, pytest.raises(grpc.RpcError) as failure:
        client.Embed(service_pb2.EmbedRequest(schema_version="v1", scope=_scope(), texts=[]))
    assert failure.value.code() is grpc.StatusCode.INVALID_ARGUMENT
    payload = dict(failure.value.trailing_metadata() or ())
    envelope = json.loads(payload["quansio-error-bin"])
    assert envelope["code"] == "VALIDATION_SCHEMA"
    assert "no text to embed" in envelope["message"]
