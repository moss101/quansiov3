"""CAP-008 in the plane: verified experience becomes evaluated candidates, never production.

These tests drive the whole proposal — patterns, candidate patches, deterministic evaluation
against the skill's suite, the INT-010 gate from `config/evolution.yaml` — directly and over
the real gRPC `ProposeSkillCandidates` boundary the runtime calls.
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
from intelligence.skills.evolution import (
    EvidenceKind,
    EvidenceRecord,
    EvolutionError,
    evaluate_candidate,
    propose_candidates,
    synthesized_suite,
)
from intelligence.skills.evolution.propose import evolution_settings
from intelligence.skills.models import SkillStatus, SkillVersionRef

NOTES = {
    "instructions": "Read the file, then quote what matters.",
    "examples": ["read the notes"],
    "tool_needs": ["fs.read"],
    "capability_needs": ["read.internal"],
}


def version(
    manifest: dict[str, object] | None = None, *, version_id: str = "sklv_notes_1"
) -> SkillVersionRef:
    return SkillVersionRef.build(
        skill_id="skl_notes",
        version_id=version_id,
        skill_name="notes-reader",
        semver="1.0.0",
        status="active",
        manifest=manifest or NOTES,
    )


def records(kind: EvidenceKind, count: int = 3, **extra: str) -> list[EvidenceRecord]:
    return [
        EvidenceRecord(
            evidence_id=f"evd_{kind.value}_{index}",
            skill_id="skl_notes",
            run_id=f"run_{kind.value}_{index}",
            kind=kind,
            digest=f"{index:064x}",
            **extra,
        )
        for index in range(count)
    ]


def settings():
    return evolution_settings()


# -- the proposal --------------------------------------------------------------------------


def test_a_recovery_pattern_becomes_an_evaluated_candidate_linked_to_its_evidence() -> None:
    thresholds, min_repeats = settings()
    evidence = records(EvidenceKind.RECOVERY, detail="retried the read after TOOL_TIMEOUT")
    (proposal,) = propose_candidates(evidence, [version()], thresholds=thresholds, min_repeats=min_repeats)
    patch = proposal.patch
    assert patch.proposed.status is SkillStatus.CANDIDATE
    assert patch.proposed.semver == "1.0.1"
    assert "Recovered before (3 verified runs): retried the read after TOOL_TIMEOUT" in (
        patch.proposed.manifest.recovery_guidance or ""
    )
    provenance = proposal.provenance
    assert provenance["origin"] == "evolution" and provenance["from_version_id"] == "sklv_notes_1"
    assert [item["evidence_id"] for item in provenance["evidence"]] == [r.evidence_id for r in evidence]
    assert [item["digest"] for item in provenance["evidence"]] == [r.digest for r in evidence]
    evaluation = provenance["evaluation"]
    assert evaluation["passed"] is True and evaluation["regressions"] == 0
    assert evaluation["eval_run_id"] == patch.eval_run_id and len(evaluation["digest"]) == 64


def test_a_success_pattern_teaches_its_verified_tasks_as_examples() -> None:
    thresholds, min_repeats = settings()
    evidence = records(EvidenceKind.SUCCESS, task="summarise the release notes")
    (proposal,) = propose_candidates(evidence, [version()], thresholds=thresholds, min_repeats=min_repeats)
    assert proposal.patch.proposed.manifest.examples == ("read the notes", "summarise the release notes")
    assert proposal.provenance["evaluation"]["passed"] is True


def test_fewer_than_min_repeats_or_nothing_new_proposes_nothing() -> None:
    thresholds, min_repeats = settings()
    assert min_repeats >= 2
    few = records(EvidenceKind.FAILURE, count=min_repeats - 1, detail="the file was missing")
    assert propose_candidates(few, [version()], thresholds=thresholds, min_repeats=min_repeats) == ()
    known = records(EvidenceKind.SUCCESS, task="read the notes")  # already an example
    assert propose_candidates(known, [version()], thresholds=thresholds, min_repeats=min_repeats) == ()


def test_records_for_a_skill_without_an_active_version_are_refused() -> None:
    thresholds, min_repeats = settings()
    with pytest.raises(EvolutionError):
        propose_candidates(
            records(EvidenceKind.FAILURE, detail="x"), [], thresholds=thresholds, min_repeats=min_repeats
        )


# -- regression rejection ------------------------------------------------------------------


def test_a_candidate_that_widens_its_needs_is_gated() -> None:
    thresholds, _ = settings()
    widened = version({**NOTES, "tool_needs": ["fs.read", "fs.write"]}, version_id="proposed")
    evaluation = evaluate_candidate(version(), widened, run_id="eval_widened", thresholds=thresholds)
    assert evaluation.widened_needs == ("fs.write",)
    assert evaluation.verdict.promotion_blocked and not evaluation.passed


def test_a_candidate_that_loses_a_case_its_version_passes_is_gated() -> None:
    thresholds, _ = settings()
    lost = version({**NOTES, "examples": ["something else entirely"]}, version_id="proposed")
    evaluation = evaluate_candidate(version(), lost, run_id="eval_lost", thresholds=thresholds)
    assert evaluation.regressions >= 1
    assert evaluation.verdict.promotion_blocked and not evaluation.passed
    assert "skill.regressions" in evaluation.verdict.reason


def test_the_synthesized_suite_holds_a_version_to_its_declared_behaviour() -> None:
    cases = {case.case_id: case for case in synthesized_suite(version())}
    assert cases["example_1_resolves"].expect_resolve
    assert not cases["without_fs_read_excluded"].expect_resolve
    assert not cases["without_read_internal_excluded"].expect_resolve
    thresholds, _ = settings()
    same = evaluate_candidate(
        version(), version(version_id="proposed"), run_id="eval_same", thresholds=thresholds
    )
    assert same.passed and same.regressions == 0


def test_a_pack_skill_is_evaluated_against_its_packs_own_suite() -> None:
    coding = SkillVersionRef.build(
        skill_id="skl_coding",
        version_id="sklv_coding_1",
        skill_name="coding",
        semver="1.0.0",
        status="active",
        manifest={
            "instructions": "patch",
            "examples": ["implement the change"],
            "tool_needs": ["fs.read", "fs.patch", "terminal.exec"],
            "capability_needs": ["read.internal", "fs.write.workspace", "process.exec.sandboxed"],
            "eval_suite_id": "eval_coding",
        },
    )
    thresholds, _ = settings()
    evaluation = evaluate_candidate(coding, coding, run_id="eval_pack", thresholds=thresholds)
    assert {outcome.case_id for outcome in evaluation.run.outcomes} == {
        "permitted_resolves",
        "missing_tool_excluded",
        "missing_capability_excluded",
    }


# -- over the wire -------------------------------------------------------------------------


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
    evidence: list[dict[str, object]], versions: list[dict[str, object]]
) -> service_pb2.SkillCandidatesRequest:
    return service_pb2.SkillCandidatesRequest(
        schema_version="v1",
        scope=service_pb2.ScopeContext(
            schema_version="v1",
            tenant_id="tn_01J8Z3K6F1N8VQ2X5W9YCAP008",
            workspace_id="ws_01J8Z3K6F1N8VQ2X5W9YCAP008",
            correlation_id="corr_cap008",
            deadline_ms=int(time.time() * 1000) + 30_000,
            capability_projection_id="cap_01J8Z3K6F1N8VQ2X5W9YCAP008",
        ),
        evidence_records_json=[json.dumps(record) for record in evidence],
        active_versions_json=[json.dumps(entry) for entry in versions],
    )


def wire_version() -> dict[str, object]:
    return {
        "skill_id": "skl_notes",
        "version_id": "sklv_notes_1",
        "name": "notes-reader",
        "semver": "1.0.0",
        "status": "active",
        "manifest": NOTES,
    }


def test_propose_skill_candidates_over_grpc() -> None:
    evidence = [
        {
            "evidence_id": f"evd_{index}",
            "skill_id": "skl_notes",
            "run_id": f"run_{index}",
            "kind": "failure",
            "digest": f"{index:064x}",
            "detail": "the notes file was missing",
        }
        for index in range(3)
    ]
    with running_gateway() as client:
        response = client.ProposeSkillCandidates(request(evidence, [wire_version()]))
        (raw,) = response.candidates_json
        candidate = json.loads(raw)
        assert candidate["skill_id"] == "skl_notes" and candidate["from_version_id"] == "sklv_notes_1"
        assert candidate["semver"] == "1.0.1"
        assert "Known failure (3 verified runs)" in candidate["manifest"]["recovery_guidance"]
        assert candidate["provenance"]["evaluation"]["passed"] is True
        assert len(candidate["provenance"]["evidence"]) == 3

        bad = dict(evidence[0], kind="gossip")
        with pytest.raises(grpc.RpcError) as refused:
            client.ProposeSkillCandidates(request([bad], [wire_version()]))
        assert refused.value.code() == grpc.StatusCode.INVALID_ARGUMENT
        assert decode_error(refused.value).code.value == "VALIDATION_SCHEMA"
