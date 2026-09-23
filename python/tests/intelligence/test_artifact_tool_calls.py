"""CAP-003: the intelligence plane proposes governed artifact tool calls, never invalid sources.

* The shared corpus (``packs/skills/artifacts/fixtures/corpus.yaml``) gets the verdict the
  Rust authority reaches (``crates/server/tests/artifact_workflows.rs`` runs the same corpus).
* ``create_call`` / ``update_call`` produce exactly the arguments ``config/tools.yaml`` accepts
  for ``artifact.create`` / ``artifact.update``, bound to the source's digest.
* A malformed source raises before any call exists; an edit round-trips to a new digest.
"""

from __future__ import annotations

import base64
import hashlib
from pathlib import Path

import pytest
import yaml
from jsonschema import Draft202012Validator

from intelligence.artifacts import (
    CREATE_TOOL,
    UPDATE_TOOL,
    ArtifactError,
    ArtifactKind,
    DocumentSource,
    check_source,
    create_call,
    parse_document,
    update_call,
)

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "packs" / "skills" / "artifacts" / "fixtures"


def corpus() -> list[dict[str, object]]:
    return yaml.safe_load((FIXTURES / "corpus.yaml").read_text())["sources"]


def tool_schema(name: str) -> dict[str, object]:
    tools = yaml.safe_load((ROOT / "config" / "tools.yaml").read_text())["tools"]
    return next(tool for tool in tools if tool["name"] == name)["input_schema"]


@pytest.mark.parametrize("entry", corpus(), ids=lambda entry: f"{entry['file']}:{entry['media_type']}")
def test_the_corpus_verdicts_are_the_authoritys(entry: dict[str, object]) -> None:
    payload = (FIXTURES / str(entry["file"])).read_bytes()
    report = check_source(ArtifactKind.parse(str(entry["kind"])), payload, str(entry["media_type"]))
    assert report.ok is entry["ok"], report.detail
    if not entry["ok"]:
        assert report.rule_id == entry["rule"], report.detail


def test_every_valid_fixture_becomes_a_create_call_the_registry_accepts() -> None:
    validator = Draft202012Validator(tool_schema(CREATE_TOOL))
    for entry in corpus():
        if not entry["ok"]:
            continue
        call = create_call(
            kind=str(entry["kind"]),
            title=f"from {entry['file']}",
            role="report",
            source=(FIXTURES / str(entry["file"])).read_bytes(),
            media_type=str(entry["media_type"]),
        )
        assert call.tool == CREATE_TOOL
        validator.validate(dict(call.arguments))
        content = str(call.arguments["content"]).encode("utf-8")
        assert hashlib.sha256(content).hexdigest() == call.content_digest
        assert call.check.ok


def test_a_malformed_source_is_never_proposed() -> None:
    for entry in corpus():
        if entry["ok"]:
            continue
        with pytest.raises(ArtifactError) as refused:
            create_call(
                kind=str(entry["kind"]),
                title="bad",
                role="report",
                source=(FIXTURES / str(entry["file"])).read_bytes(),
                media_type=str(entry["media_type"]),
            )
        assert refused.value.rule_id == entry["rule"]


def test_an_edit_round_trips_to_a_new_version_call() -> None:
    first = create_call(
        kind=ArtifactKind.DOCUMENT,
        title="brief",
        role="report",
        source=DocumentSource(title="Brief", body="Draft one."),
    )
    parsed = parse_document(str(first.arguments["content"]).encode("utf-8"))
    edited = DocumentSource(title=parsed.title, body=parsed.body + "\n\nA second paragraph.")
    second = update_call(
        artifact_id="art_01J8Z3K6F1N8VQ2X5W9Y0ABCDE", kind=ArtifactKind.DOCUMENT, source=edited
    )
    assert second.tool == UPDATE_TOOL
    Draft202012Validator(tool_schema(UPDATE_TOOL)).validate(dict(second.arguments))
    assert second.content_digest != first.content_digest
    assert parse_document(str(second.arguments["content"]).encode("utf-8")).body.endswith(
        "A second paragraph."
    )
    with pytest.raises(ArtifactError):
        update_call(
            artifact_id="art_01J8Z3K6F1N8VQ2X5W9Y0ABCDE", kind=ArtifactKind.DOCUMENT, source="# Brief\n"
        )


def test_media_travels_as_base64_bound_to_its_digest() -> None:
    png = b"\x89PNG\r\n\x1a\n" + bytes(64)
    call = create_call(kind="image", title="chart", role="figure", source=png, media_type="image/png")
    Draft202012Validator(tool_schema(CREATE_TOOL)).validate(dict(call.arguments))
    assert "content" not in call.arguments
    assert base64.b64decode(str(call.arguments["content_base64"])) == png
    assert call.content_digest == hashlib.sha256(png).hexdigest()
