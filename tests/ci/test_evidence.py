"""Tests for the task evidence collector (scripts/dev/evidence.py)."""
from __future__ import annotations

import json
from datetime import datetime, timezone

from scripts.dev import evidence


def test_utc_stamp_is_dos19_directory_format():
    stamp = evidence.utc_stamp(datetime(2026, 9, 12, 4, 5, 6, tzinfo=timezone.utc))
    assert stamp == "2026-09-12T04-05-06Z"


def test_collect_captures_commands_and_hashes_artifacts(tmp_path):
    artifact = tmp_path / "artifact.txt"
    artifact.write_text("payload")
    summary = evidence.collect(
        "TEST-001",
        ["echo hello", "exit 3"],
        ["artifact.txt"],
        out_dir=tmp_path / "evidence",
        root=tmp_path,
        stamp="2026-09-12T00-00-00Z",
    )
    bundle = tmp_path / "evidence" / "TEST-001" / "2026-09-12T00-00-00Z"
    assert (bundle / "summary.json").exists()
    assert summary["task_id"] == "TEST-001"
    assert [c["exit_code"] for c in summary["commands"]] == [0, 3]
    assert [c["result"] for c in summary["commands"]] == ["PASS", "FAIL"]
    # Raw output is preserved for later audit.
    assert "hello" in (bundle / "command-1.log").read_text()
    assert summary["artifacts"][0]["sha256"] == evidence.sha256_file(artifact)


def test_collect_records_missing_artifact_as_unhashed(tmp_path):
    summary = evidence.collect(
        "TEST-003", [], ["does-not-exist.txt"], out_dir=tmp_path / "ev", root=tmp_path
    )
    assert summary["artifacts"] == [{"path": "does-not-exist.txt", "sha256": None, "bytes": None}]


def test_collect_failure_exit_code(tmp_path):
    rc = evidence.main(
        ["--task", "TEST-002", "--command", "exit 7", "--out-dir", str(tmp_path / "ev")]
    )
    assert rc == 1
    summary = json.loads(
        next((tmp_path / "ev" / "TEST-002").glob("*/summary.json")).read_text()
    )
    assert summary["results"][0]["exit_code"] == 7
