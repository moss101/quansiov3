"""The `ProposeMemory` wire, end to end onto real PostgreSQL (INT-007 unit 3).

This drives the shipped RPC path rather than the owner in isolation: a real `IntelligenceServer` over
loopback TCP whose servicer holds the production-shaped `StoreMemoryProposals` sink over a real
`SqlMemoryStore`, so the call the runtime makes is the call tested — scope gate, candidate
translation, owner decision, and a row in `public.memory_entries`.

Environment: ``QUANSIO_TEST_POSTGRES_URL`` is the superuser DSN used to create a scratch database.
Absent → the suite reports ``BLOCKED_EXTERNAL`` and skips.
"""

from __future__ import annotations

import contextlib
import json
import os
import subprocess
import sys
import time
from collections.abc import Iterator
from pathlib import Path

import grpc
import psycopg
import pytest
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.memory.candidates import StoreMemoryProposals
from intelligence.memory.models import MemoryProvenance, MemoryStatus
from intelligence.memory.store import SqlMemoryStore, memory_for
from intelligence.server import (
    ErrorCode,
    IntelligenceGatewayServicer,
    IntelligenceServer,
    ServerConfig,
    Transport,
    decode_error,
)

ROOT = Path(__file__).resolve().parents[3]
ADMIN_URL = os.environ.get("QUANSIO_TEST_POSTGRES_URL", "").strip()
SEEDER = ROOT / "scripts" / "dev" / "seed_test_database.py"

pytestmark = pytest.mark.skipif(
    not ADMIN_URL,
    reason="BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running",
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
SUBJECT = "usr_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
CORRELATION_ID = "corr_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
CAPABILITY_PROJECTION_ID = "cp_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"


def seed_scratch_database(name: str, *, tenants: list[str], workspaces: list[str]) -> dict:
    result = subprocess.run(
        [
            sys.executable,
            str(SEEDER),
            "--admin-url",
            ADMIN_URL,
            "--database",
            name,
            *(f"--tenant={tenant}" for tenant in tenants),
            *(f"--workspace={workspace}" for workspace in workspaces),
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(f"seeding {name} failed:\n{result.stdout}\n{result.stderr}")
    return json.loads(result.stdout.strip().splitlines()[-1])


def drop_scratch_database(name: str) -> None:
    with psycopg.connect(ADMIN_URL, autocommit=True) as admin:
        admin.execute(f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")


@pytest.fixture(scope="module")
def database() -> Iterator[str]:
    name = f"quansio_pymemp_{os.getpid()}"
    seeded = seed_scratch_database(name, tenants=[TENANT], workspaces=[WORKSPACE])
    try:
        yield seeded["url"]
    finally:
        drop_scratch_database(name)


@pytest.fixture(autouse=True)
def empty_store(database: str) -> Iterator[None]:
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("DELETE FROM memory_entries")
    yield


def scope(deadline_ms: int, *, tenant_id: str = TENANT, workspace_id: str = WORKSPACE):
    return service_pb2.ScopeContext(
        schema_version="v1",
        tenant_id=tenant_id,
        workspace_id=workspace_id,
        correlation_id=CORRELATION_ID,
        deadline_ms=deadline_ms,
        capability_projection_id=CAPABILITY_PROJECTION_ID,
    )


def wire_candidate(**overrides: object) -> service_pb2.MemoryCandidate:
    values: dict[str, object] = {
        "schema_version": "v1",
        "scope": scope(int(time.time() * 1000) + 30_000),
        "subject_ref": SUBJECT,
        "content": "Prefers concise summaries over long prose.",
        "provenance_kind": MemoryProvenance.EXPLICIT_USER.value,
        "provenance_ref": "msg_01J8Z3K6F1N8VQ2X5W9Y0MMMMM",
        "confidence": 0.8,
    }
    values.update(overrides)
    return service_pb2.MemoryCandidate(**values)  # type: ignore[arg-type]


@contextlib.contextmanager
def running_gateway(
    database: str | None,
) -> Iterator[service_pb2_grpc.IntelligenceGatewayStub]:
    memories = (
        StoreMemoryProposals(store=SqlMemoryStore(lambda: psycopg.connect(database)))
        if database is not None
        else None
    )
    server = IntelligenceServer(
        ServerConfig(transport=Transport.TCP, host="127.0.0.1", port=0),
        servicer=IntelligenceGatewayServicer(memories=memories),
    )
    address = server.start()
    channel = grpc.insecure_channel(address)
    try:
        grpc.channel_ready_future(channel).result(timeout=10.0)
        yield service_pb2_grpc.IntelligenceGatewayStub(channel)
    finally:
        channel.close()
        server.stop(2.0)


def fabric(database: str):
    return memory_for(SqlMemoryStore(lambda: psycopg.connect(database)), tenant_id=TENANT)


def test_a_proposal_over_the_wire_lands_as_a_candidate_row(database: str) -> None:
    with running_gateway(database) as client:
        ack = client.ProposeMemory(wire_candidate())
    assert ack.accepted
    assert ack.proposal_id.startswith("mem_")
    assert ack.reason == ""

    stored = fabric(database).get(ack.proposal_id)
    assert stored.status is MemoryStatus.CANDIDATE, "a proposal is never remembered immediately"
    assert stored.subject_ref == SUBJECT
    assert stored.provenance_kind is MemoryProvenance.EXPLICIT_USER
    assert stored.provenance_ref == "msg_01J8Z3K6F1N8VQ2X5W9Y0MMMMM"
    assert stored.confidence == 0.8
    assert stored.scope.value == "workspace", "the call's workspace decides the scope"
    assert stored.workspace_id == WORKSPACE
    assert stored.tenant_id == TENANT, "the call's scope decides the tenant, not the candidate"
    assert fabric(database).retrievable("2026-09-13T12:00:00Z") == ()


def test_the_same_claim_over_the_wire_is_recognised_not_duplicated(database: str) -> None:
    with running_gateway(database) as client:
        first = client.ProposeMemory(wire_candidate())
        second = client.ProposeMemory(wire_candidate())
    assert first.proposal_id == second.proposal_id
    assert second.reason == "already remembered"
    assert fabric(database).count() == 1


def test_a_refused_provenance_kind_is_a_typed_error(database: str) -> None:
    with running_gateway(database) as client, pytest.raises(grpc.RpcError) as failure:
        client.ProposeMemory(wire_candidate(provenance_kind="the_model_thought_so"))
    envelope = decode_error(failure.value)
    assert envelope.code is ErrorCode.VALIDATION_SCHEMA
    assert "provenance_kind" in envelope.message
    assert fabric(database).count() == 0, "nothing is stored for a refused candidate"


def test_an_empty_candidate_is_refused(database: str) -> None:
    with running_gateway(database) as client, pytest.raises(grpc.RpcError) as failure:
        client.ProposeMemory(wire_candidate(content="   "))
    envelope = decode_error(failure.value)
    assert envelope.code is ErrorCode.VALIDATION_SCHEMA
    assert ("owner_task", "INT-007") in {(detail.key, detail.value) for detail in envelope.details}
    assert fabric(database).count() == 0


def test_the_scope_gate_runs_before_anything_is_stored(database: str) -> None:
    with running_gateway(database) as client, pytest.raises(grpc.RpcError) as failure:
        client.ProposeMemory(wire_candidate(scope=scope(int(time.time() * 1000) + 30_000, tenant_id="")))
    assert decode_error(failure.value).code is ErrorCode.VALIDATION_SCHEMA
    assert "tenant_id" in decode_error(failure.value).message
    assert fabric(database).count() == 0


def test_a_process_with_no_memory_sink_fails_closed() -> None:
    """A candidate is never acknowledged while nothing would store it."""
    with running_gateway(None) as client, pytest.raises(grpc.RpcError) as failure:
        client.ProposeMemory(wire_candidate())
    envelope = decode_error(failure.value)
    assert envelope.code is ErrorCode.ROUTE_UNAVAILABLE
    assert "composition root" in envelope.message
    assert ("owner_task", "INT-007") in {(detail.key, detail.value) for detail in envelope.details}


def test_a_promoted_memory_becomes_retrievable(database: str) -> None:
    """The whole path: propose over the wire, promote, and retrieval finds it."""
    with running_gateway(database) as client:
        ack = client.ProposeMemory(wire_candidate())
    bound = fabric(database)
    bound.set_status(ack.proposal_id, MemoryStatus.ACTIVE)
    retrieved = bound.retrievable("2026-09-13T12:00:00Z")
    assert [item.id for item in retrieved] == [ack.proposal_id]
    bound.mark_used(ack.proposal_id, "2026-09-13T12:00:00Z")
    assert bound.get(ack.proposal_id).last_used_at == "2026-09-13T12:00:00Z"
