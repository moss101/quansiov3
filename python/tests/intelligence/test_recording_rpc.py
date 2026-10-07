"""ADP-003 over the real gRPC boundary: ``CompileSkillRecording`` returns a proposal and nothing else.

The plane is a proposal reader: it never writes, it refuses an unredacted projection with a typed error that
names categories and places but never the text, and the proposal that comes back carries its untrusted
origin and passes the validator the server runs again.
"""

from __future__ import annotations

import contextlib
import json
import time
from collections.abc import Iterator

import grpc
import pytest
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.server import (
    IntelligenceGatewayServicer,
    IntelligenceServer,
    ServerConfig,
    Transport,
    decode_error,
)
from intelligence.skills.recording import ToolInfo, validate_proposal

RECORDING = "rec_01J8Z3K6F1N8VQ2X5W9YADP003"
NOTES = {"bundle_id": "com.apple.Notes", "name": "Notes"}
TOOLS = [
    {"name": "computer.read", "description": "read", "effect_classes": ["read.internal"]},
    {"name": "computer.click", "description": "click", "effect_classes": ["computer.input.privileged"]},
    {"name": "computer.type", "description": "type", "effect_classes": ["computer.input.privileged"]},
]


@contextlib.contextmanager
def running_gateway() -> Iterator[service_pb2_grpc.IntelligenceGatewayStub]:
    server = IntelligenceServer(
        ServerConfig(transport=Transport.TCP, host="127.0.0.1", port=0),
        servicer=IntelligenceGatewayServicer(),
    )
    address = server.start()
    channel = grpc.insecure_channel(address)
    try:
        grpc.channel_ready_future(channel).result(timeout=10.0)
        yield service_pb2_grpc.IntelligenceGatewayStub(channel)
    finally:
        channel.close()
        server.stop(2.0)


def projection(text: str = "Quarterly report") -> dict[str, object]:
    return {
        "apps": [NOTES],
        "actions": [
            {
                "seq": 1,
                "at": "t",
                "kind": "click",
                "app": NOTES,
                "target": {"ax_role": "AXButton", "ax_title": "New Note"},
            },
            {
                "seq": 2,
                "at": "t",
                "kind": "type",
                "app": NOTES,
                "target": {"ax_role": "AXTextArea", "ax_title": "Body"},
                "text": text,
            },
        ],
        "transcript": [],
        "screen_text": [],
        "paused": [],
        "redaction": {"rule_set": "v1", "counts": {}},
    }


def request(proj: object, recording_id: str = RECORDING) -> service_pb2.CompileRecordingRequest:
    return service_pb2.CompileRecordingRequest(
        schema_version="v1",
        scope=service_pb2.ScopeContext(
            schema_version="v1",
            tenant_id="tn_01J8Z3K6F1N8VQ2X5W9YADP003",
            workspace_id="ws_01J8Z3K6F1N8VQ2X5W9YADP003",
            correlation_id="corr_adp003",
            deadline_ms=int(time.time() * 1000) + 30_000,
            capability_projection_id="rbac:author_work",
        ),
        recording_id=recording_id,
        projection_json=json.dumps(proj),
        tools_json=[json.dumps(tool) for tool in TOOLS],
        settings_json=json.dumps(
            {"proposal": {"max_bytes": 262144, "max_steps": 200, "max_slots": 64}, "tool_paths": []}
        ),
    )


def test_a_proposal_comes_back_over_the_wire_and_passes_the_validator() -> None:
    with running_gateway() as client:
        response = client.CompileSkillRecording(request(projection()))
    proposal = json.loads(response.proposal_json)
    assert proposal["trust"] == {
        "source": "recording",
        "level": "UNTRUSTED_EXTERNAL",
        "recording_id": RECORDING,
        "injection_suspected": [],
    }
    validate_proposal(proposal, recording_id=RECORDING, tools=[ToolInfo.parse(json.dumps(t)) for t in TOOLS])
    assert proposal["slots"][0]["classification"] is None


def test_an_unredacted_projection_is_refused_with_a_typed_error_that_never_names_the_text() -> None:
    with running_gateway() as client, pytest.raises(grpc.RpcError) as refused:
        client.CompileSkillRecording(request(projection("AKIAIOSFODNN7EXAMPLE")))
    assert refused.value.code() == grpc.StatusCode.FAILED_PRECONDITION
    error = decode_error(refused.value)
    assert error.code.value == "RECORDING_UNREDACTED"
    assert "AKIA" not in error.message and "api_key" in error.message


def test_a_malformed_request_is_a_schema_refusal_not_a_crash() -> None:
    with running_gateway() as client:
        bad = request(projection())
        bad.projection_json = "{not json"
        with pytest.raises(grpc.RpcError) as refused:
            client.CompileSkillRecording(bad)
        assert decode_error(refused.value).code.value == "VALIDATION_SCHEMA"
        empty = request(projection(), recording_id="")
        with pytest.raises(grpc.RpcError) as refused:
            client.CompileSkillRecording(empty)
        assert decode_error(refused.value).code.value == "VALIDATION_SCHEMA"


def test_the_servicer_has_no_write_path_for_recordings() -> None:
    names = {name for name in dir(IntelligenceGatewayServicer) if not name.startswith("_")}
    recording_methods = {name for name in names if "ecording" in name}
    assert recording_methods == {"CompileSkillRecording"}
