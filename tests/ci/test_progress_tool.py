"""Tests for the progress-registry editor (scripts/dev/progress.py)."""
from __future__ import annotations

import json

import pytest

from scripts.dev import progress as progress_tool


def _fixture(tmp_path):
    path = tmp_path / "progress.json"
    path.write_text(
        json.dumps(
            {
                "version": "8.1",
                "updated_at": "2026-09-12",
                "tasks": {
                    "GOV-001": {
                        "coverage": None,
                        "status": "NOT_STARTED",
                        "claimed_by": None,
                        "started_at": None,
                        "updated_at": None,
                        "git_commit": None,
                        "tests": [],
                        "artifacts": [],
                        "real_boundary_evidence": [],
                        "implementation_complete": False,
                        "blocker": None,
                        "notes": "",
                    }
                },
            }
        )
    )
    return path


def _read(path):
    return json.loads(path.read_text())["tasks"]["GOV-001"]


def test_claim_sets_status_owner_and_timestamps(tmp_path):
    path = _fixture(tmp_path)
    entry = progress_tool.update(
        "GOV-001",
        status="RECONCILING",
        claimed_by="agent:principal-1",
        progress_path=path,
        now="2026-09-12T04:00:00Z",
    )
    assert entry["status"] == "RECONCILING"
    assert entry["claimed_by"] == "agent:principal-1"
    assert entry["started_at"] == "2026-09-12T04:00:00Z"
    assert entry["updated_at"] == "2026-09-12T04:00:00Z"
    assert _read(path)["started_at"] == "2026-09-12T04:00:00Z"


def test_pass_records_commit_tests_artifacts(tmp_path):
    path = _fixture(tmp_path)
    entry = progress_tool.update(
        "GOV-001",
        status="PASS",
        coverage="GENUINE_GAP",
        git_commit="a" * 40,
        tests=["python3 -m pytest tests/architecture"],
        artifacts=["evidence/GOV-001/2026-09-12T04-00-00Z/summary.json"],
        implementation_complete=True,
        progress_path=path,
        now="2026-09-12T04:30:00Z",
    )
    assert entry["coverage"] == "GENUINE_GAP"
    assert entry["git_commit"] == "a" * 40
    assert entry["implementation_complete"] is True
    assert entry["tests"] and entry["artifacts"]


def test_started_at_is_preserved_on_later_updates(tmp_path):
    path = _fixture(tmp_path)
    progress_tool.update("GOV-001", status="IN_PROGRESS", claimed_by="a", progress_path=path, now="2026-09-12T04:00:00Z")
    entry = progress_tool.update("GOV-001", status="PASS", progress_path=path, now="2026-09-12T05:00:00Z")
    assert entry["started_at"] == "2026-09-12T04:00:00Z"
    assert entry["updated_at"] == "2026-09-12T05:00:00Z"


def test_invalid_status_and_coverage_rejected(tmp_path):
    path = _fixture(tmp_path)
    with pytest.raises(ValueError):
        progress_tool.update("GOV-001", status="VERIFIED", progress_path=path)
    with pytest.raises(ValueError):
        progress_tool.update("GOV-001", coverage="NOPE", progress_path=path)


def test_unknown_task_rejected(tmp_path):
    with pytest.raises(KeyError):
        progress_tool.update("GOV-999", status="IN_PROGRESS", progress_path=_fixture(tmp_path))


def test_not_started_cannot_carry_evidence(tmp_path):
    with pytest.raises(ValueError):
        progress_tool.update("GOV-001", status="NOT_STARTED", git_commit="b" * 8, progress_path=_fixture(tmp_path))
