"""Pack install in the plane (ADP-008, DOMAIN.md 11.6): content-hash revisions, verification, compilation.

The plane verifies and compiles a pack and writes nothing. These tests pin:

* the revision is a content address: stable under key order and under a re-review, changed by any change
  to what executes, and identical to the server's (a shared corpus);
* verification refuses what CAP-004 and the quarantine record refuse, and a finding names where, never what;
* compilation lets a pack name only tools a model can see: a host-only tool, or one the registry never
  declared, fails the compile, so a pack cannot widen the model's reach;
* all of it holds over the real gRPC boundary.
"""

from __future__ import annotations

import contextlib
import copy
import json
import time
from collections.abc import Iterator
from pathlib import Path

import grpc
import pytest
import yaml
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.capability_compiler import (
    InstallSettings,
    ToolVisibility,
    compile_install,
    manifest_digest,
    revision_of,
)
from intelligence.capability_compiler.install import (
    PHASE_COMPILE,
    PHASE_VERIFY,
    PackContentError,
    canonical_json,
)
from intelligence.server import (
    IntelligenceGatewayServicer,
    IntelligenceServer,
    ServerConfig,
    Transport,
    decode_error,
)

ROOT = Path(__file__).resolve().parents[3]
CANARY = "qncy_test_canary_not_for_prod"
TOOLS = [
    ToolVisibility("fs.read", True),
    ToolVisibility("fs.patch", True),
    ToolVisibility("terminal.exec", True),
    ToolVisibility("webdev.preview_scopes", False, True),
]


def builtin(name: str) -> dict[str, object]:
    return yaml.safe_load((ROOT / "packs" / "capabilities" / name / "pack.yaml").read_text(encoding="utf-8"))


def registry_tools() -> list[ToolVisibility]:
    document = yaml.safe_load((ROOT / "config" / "tools.yaml").read_text(encoding="utf-8"))
    return [
        ToolVisibility(entry["name"], entry.get("model_visible", True), entry.get("host_callable", False))
        for entry in document["tools"]
    ]


def rules(compilation, phase: str | None = None) -> list[str]:  # type: ignore[no-untyped-def]
    return [item.rule for item in compilation.diagnostics if phase is None or item.phase == phase]


def pack() -> dict[str, object]:
    return builtin("coding")


# ---- revision ---------------------------------------------------------------------------------


def test_the_revision_is_a_content_address() -> None:
    manifest, files = pack(), {"notes/readme.md": "hello"}
    base = revision_of(manifest, files)
    assert base.startswith("sha256:") and len(base) == 71
    # Key order is not content.
    shuffled = dict(reversed(list(manifest.items())))
    assert revision_of(shuffled, dict(reversed(list(files.items())))) == base
    # A re-review changes the record, not what executes.
    reviewed = copy.deepcopy(manifest)
    reviewed["review"]["reviewed_at"] = "2030-01-01T00:00:00Z"  # type: ignore[index]
    reviewed["provenance"]["digest"] = "sha256:" + "a" * 64  # type: ignore[index]
    assert revision_of(reviewed, files) == base
    # Anything that executes is content: a field, a tool, a file.
    for edit in (
        lambda m: m.update(semver="1.0.1"),
        lambda m: m["contents"]["tool_names"].append("fs.write"),  # type: ignore[index]
        lambda m: m["provenance"].update(source="elsewhere"),  # type: ignore[union-attr]
    ):
        changed = copy.deepcopy(manifest)
        edit(changed)
        assert revision_of(changed, files) != base
    assert revision_of(manifest, {"notes/readme.md": "hello!"}) != base
    assert revision_of(manifest, {**files, "extra.txt": ""}) != base


def test_a_float_cannot_be_content_because_two_implementations_would_disagree() -> None:
    manifest = pack()
    manifest["contents"]["compatibility"] = [1.5]  # type: ignore[index]
    with pytest.raises(PackContentError):
        revision_of(manifest, {})
    assert "pack.schema" in rules(compile_install(manifest, {}, TOOLS))


def test_the_revision_corpus_pins_the_plane_to_the_server() -> None:
    corpus = json.loads(
        (ROOT / "tests/contract/fixtures/packs/revision_corpus.json").read_text(encoding="utf-8")
    )
    assert len(corpus["cases"]) >= 4
    for case in corpus["cases"]:
        content = {
            "files": case["files"],
            "manifest": __import__(
                "intelligence.capability_compiler.install", fromlist=["content_manifest"]
            ).content_manifest(case["manifest"]),
        }
        assert canonical_json(content) == case["canonical"], case["name"]
        assert revision_of(case["manifest"], case["files"]) == case["revision"], case["name"]
        assert manifest_digest(case["manifest"]) == case["manifest_digest"], case["name"]


@pytest.mark.parametrize(
    "name", sorted(p.name for p in (ROOT / "packs" / "capabilities").iterdir() if p.is_dir())
)
def test_every_builtin_pack_verifies_and_compiles_against_the_real_registry(name: str) -> None:
    manifest = builtin(name)
    compilation = compile_install(manifest, {}, registry_tools())
    assert compilation.ok, [item.as_mapping() for item in compilation.diagnostics]
    # The recorded digest (CAP-007) is the manifest digest: the same rule, computed again here.
    assert compilation.manifest_digest == manifest["provenance"]["digest"]  # type: ignore[index]
    assert compilation.revision == revision_of(manifest, {})
    declared = manifest["contents"]["tool_names"]  # type: ignore[index]
    assert compilation.resolution["tools"] == sorted(declared)
    assert compilation.resolution["widened"] is False


# ---- verify -----------------------------------------------------------------------------------


def test_a_digest_that_does_not_name_the_content_is_refused() -> None:
    manifest = pack()
    manifest["contents"]["examples"].append("edited after review")  # type: ignore[index]
    compilation = compile_install(manifest, {}, TOOLS)
    assert "pack.digest" in rules(compilation, PHASE_VERIFY)
    assert compilation.failed_phase == PHASE_VERIFY and not compilation.ok


def test_a_revision_claim_that_is_not_the_content_is_refused() -> None:
    manifest = pack()
    claimed = InstallSettings(expected_revision="sha256:" + "0" * 64)
    assert "pack.revision" in rules(compile_install(manifest, {}, TOOLS, claimed))
    honest = InstallSettings(expected_revision=revision_of(manifest, {}))
    assert compile_install(manifest, {}, TOOLS, honest).ok


@pytest.mark.parametrize(
    ("edit", "rule"),
    [
        (lambda m: m["quarantine"].update(status="pending"), "pack.quarantine"),
        (lambda m: m["review"].update(outcome="rejected"), "pack.quarantine"),
        (lambda m: m["evaluation"].update(result="fail"), "pack.quarantine"),
        (lambda m: m.update(installs_daemon=True), "pack.daemon"),
        (lambda m: m.update(bypasses_runtime_policy=True), "pack.bypass"),
        (lambda m: m.update(surprise=1), "pack.schema"),
        (lambda m: m.update(semver="one"), "pack.schema"),
        (lambda m: m.update(name="Bad Name"), "pack.schema"),
        (lambda m: m["contents"].update(extra=[]), "pack.schema"),
        (lambda m: m["contents"].update(tool_names="fs.read"), "pack.schema"),
    ],
)
def test_the_quarantine_record_and_the_closed_shape_are_enforced(edit, rule: str) -> None:  # type: ignore[no-untyped-def]
    manifest = pack()
    edit(manifest)
    # A digest edit would also fail; re-record it so the rule under test is the only one.
    if rule != "pack.schema":
        manifest["provenance"]["digest"] = manifest_digest(manifest)  # type: ignore[index]
    compilation = compile_install(manifest, {}, TOOLS)
    assert rule in rules(compilation, PHASE_VERIFY), [item.as_mapping() for item in compilation.diagnostics]
    assert not compilation.ok


@pytest.mark.parametrize("path", ["../escape.txt", "/abs.txt", ".hidden", "a//b.txt", "a\\b.txt", ""])
def test_a_file_path_cannot_leave_the_pack(path: str) -> None:
    compilation = compile_install(pack(), {path: "x"}, TOOLS)
    assert "pack.schema" in rules(compilation)


def test_the_bounds_are_enforced_in_the_plane_as_well() -> None:
    settings = InstallSettings(max_files=1, max_file_bytes=4, max_content_bytes=6)
    compilation = compile_install(pack(), {"a.txt": "12345", "b.txt": "1"}, TOOLS, settings)
    messages = " ".join(item.message for item in compilation.diagnostics)
    assert "at most 1 files" in messages and "at most 4 bytes" in messages


def test_a_credential_in_a_file_or_the_manifest_is_refused_and_never_echoed() -> None:
    secret = f"sk-live-{CANARY}-aaaaaaaaaaaaaaaaaaaaaaaa"
    in_file = compile_install(pack(), {"notes/howto.md": f"call the api with {secret}"}, TOOLS)
    assert "pack.credential" in rules(in_file, PHASE_VERIFY)
    manifest = pack()
    manifest["contents"]["examples"].append(f"password: hunter2-{CANARY}")  # type: ignore[index]
    manifest["provenance"]["digest"] = manifest_digest(manifest)  # type: ignore[index]
    in_manifest = compile_install(manifest, {}, TOOLS)
    assert "pack.credential" in rules(in_manifest, PHASE_VERIFY)
    for compilation in (in_file, in_manifest):
        text = json.dumps(compilation.as_mapping())
        assert CANARY not in text and "hunter2" not in text and "sk-live" not in text
        assert any(item.pointer.startswith(("/files/", "/manifest")) for item in compilation.diagnostics)


# ---- compile ----------------------------------------------------------------------------------


def test_a_pack_cannot_name_a_tool_a_model_cannot_see() -> None:
    manifest = pack()
    manifest["contents"]["tool_names"] = ["fs.read", "webdev.preview_scopes", "fs.teleport"]  # type: ignore[index]
    manifest["provenance"]["digest"] = manifest_digest(manifest)  # type: ignore[index]
    compilation = compile_install(manifest, {}, TOOLS)
    assert compilation.failed_phase == PHASE_COMPILE
    by_rule = {item.rule: item for item in compilation.diagnostics}
    assert by_rule["pack.tool.host_only"].pointer == "/contents/tool_names/1"
    assert by_rule["pack.tool.unknown"].pointer == "/contents/tool_names/2"
    # The tool a model can see still resolves; the others are not quietly dropped into success.
    assert compilation.resolution["tools"] == ["fs.read"]
    assert not compilation.ok


def test_verification_is_reported_before_compilation() -> None:
    manifest = pack()
    manifest["contents"]["tool_names"] = ["webdev.preview_scopes"]  # type: ignore[index]
    manifest["quarantine"]["status"] = "pending"  # type: ignore[index]
    manifest["provenance"]["digest"] = manifest_digest(manifest)  # type: ignore[index]
    compilation = compile_install(manifest, {}, TOOLS)
    assert compilation.failed_phase == PHASE_VERIFY
    assert {item.phase for item in compilation.diagnostics} == {PHASE_VERIFY, PHASE_COMPILE}


def test_a_duplicate_tool_is_a_warning_not_a_failure() -> None:
    manifest = pack()
    manifest["contents"]["tool_names"] = ["fs.read", "fs.read"]  # type: ignore[index]
    manifest["provenance"]["digest"] = manifest_digest(manifest)  # type: ignore[index]
    compilation = compile_install(manifest, {}, TOOLS)
    assert compilation.ok
    assert [item.severity for item in compilation.diagnostics] == ["warning"]


def test_the_evaluation_record_must_be_for_the_suite_the_pack_names() -> None:
    manifest = pack()
    manifest["evaluation"]["eval_suite_id"] = "eval_something_else"  # type: ignore[index]
    compilation = compile_install(manifest, {}, TOOLS)
    assert "pack.eval.suite" in rules(compilation, PHASE_COMPILE)


# ---- over the wire ----------------------------------------------------------------------------


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


def request(
    content: object, settings: object | None = None, request_id: str = "cmd_adp008"
) -> service_pb2.CompilePackRequest:
    return service_pb2.CompilePackRequest(
        schema_version="v1",
        scope=service_pb2.ScopeContext(
            schema_version="v1",
            tenant_id="tn_01J8Z3K6F1N8VQ2X5W9YADP008",
            workspace_id="ws_01J8Z3K6F1N8VQ2X5W9YADP008",
            correlation_id="corr_adp008",
            deadline_ms=int(time.time() * 1000) + 30_000,
            capability_projection_id="rbac:manage_tenant",
        ),
        request_id=request_id,
        pack_json=json.dumps(content),
        tools_json=[
            json.dumps({"name": t.name, "model_visible": t.model_visible, "host_callable": t.host_callable})
            for t in TOOLS
        ],
        settings_json=json.dumps(settings or {}),
    )


def test_the_plane_compiles_a_pack_over_the_real_grpc_boundary() -> None:
    manifest = pack()
    with running_gateway() as client:
        good = json.loads(client.CompilePack(request({"manifest": manifest, "files": {}})).compilation_json)
        assert good["ok"] is True and good["failed_phase"] is None
        assert good["revision"] == revision_of(manifest, {})
        assert good["resolution"]["tools"] == sorted(manifest["contents"]["tool_names"])  # type: ignore[index]
        bad = copy.deepcopy(manifest)
        bad["contents"]["tool_names"] = ["webdev.preview_scopes"]  # type: ignore[index]
        bad["provenance"]["digest"] = manifest_digest(bad)  # type: ignore[index]
        refused = json.loads(client.CompilePack(request({"manifest": bad, "files": {}})).compilation_json)
        assert refused["ok"] is False and refused["failed_phase"] == "compile"
        assert [item["rule"] for item in refused["diagnostics"]] == ["pack.tool.host_only"]


def test_a_request_the_plane_cannot_read_is_a_schema_refusal_not_a_crash() -> None:
    with running_gateway() as client:
        for broken in (
            request({"manifest": pack()}, request_id=""),
            service_pb2.CompilePackRequest(
                schema_version="v1", scope=request({}).scope, request_id="cmd_x", pack_json="{not json"
            ),
            request({"manifest": pack()}, settings={"expected_revision": "nope"}),
        ):
            with pytest.raises(grpc.RpcError) as refused:
                client.CompilePack(broken)
            assert decode_error(refused.value).code.value == "VALIDATION_SCHEMA"


def test_the_servicer_has_no_write_path_for_packs() -> None:
    names = {name for name in dir(IntelligenceGatewayServicer) if not name.startswith("_")}
    assert {name for name in names if "ack" in name and name[0].isupper()} == {"CompilePack"}
