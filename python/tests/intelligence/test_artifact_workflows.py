"""CAP-003: artifacts are versioned source files; failed format validation blocks completion.

The suite drives the shipped `create` / `edit` / `complete` path with the pack fixtures
(document, spreadsheet, slides) and a malformed payload. A completion object is only
returned when format and quality hold.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from intelligence.artifacts import (
    ArtifactError,
    ArtifactKind,
    complete,
    create,
    create_from_fixture,
    digest_of,
    edit,
    inspect,
    load_skill_manifest,
    parse_document,
    parse_kind,
    parse_presentation,
    parse_spreadsheet,
    preview,
    serialize_source,
    skill_pack_root,
    validate_format,
)
from intelligence.skills import CapabilitySnapshot, SkillStatus, SkillVersionRef, resolve

ROOT = Path(__file__).resolve().parents[3]
WORKSPACE = "ws_01ARTIFACTWORKFLOW00000001"


def test_fixtures_round_trip_document_spreadsheet_and_slides() -> None:
    """Named test: document/spreadsheet/slide fixtures parse and canonicalize."""
    document = create_from_fixture(
        "document.md", workspace_id=WORKSPACE, kind=ArtifactKind.DOCUMENT, title="brief"
    )
    spreadsheet = create_from_fixture(
        "spreadsheet.csv", workspace_id=WORKSPACE, kind=ArtifactKind.SPREADSHEET, title="metrics"
    )
    slides = create_from_fixture(
        "slides.json", workspace_id=WORKSPACE, kind=ArtifactKind.PRESENTATION, title="deck"
    )
    for record in (document, spreadsheet, slides):
        parsed = parse_kind(record.kind, record.current.body)
        assert serialize_source(parse_kind(record.kind, serialize_source(parsed))) == serialize_source(parsed)
        report, quality = inspect(record)
        assert report.ok, report.detail
        assert quality.ok, quality.detail
        done = complete(record)
        assert done.artifact_id == record.id
        assert done.version_id == record.current.id
        assert done.content_digest == record.current.content_digest
        assert done.seq == 1


def test_round_trip_edit_mints_a_new_version() -> None:
    """Named test: round-trip edit. Bytes stay immutable; seq/parent/digest move."""
    original = create_from_fixture(
        "document.md", workspace_id=WORKSPACE, kind="document", title="brief", seed="doc-edit"
    )
    first = original.current
    parsed = parse_document(first.body)
    updated_source = type(parsed)(title=parsed.title, body=parsed.body + "\n\nEdited in place of a blob.")
    edited = edit(original, serialize_source(updated_source), seed="doc-edit")
    assert edited.id == original.id
    assert len(edited.versions) == 2
    second = edited.current
    assert second.seq == 2
    assert second.parent_version_id == first.id
    assert second.id != first.id
    assert second.content_digest != first.content_digest
    assert first.body == original.current.body  # original version bytes unchanged
    assert edited.version(first.id).body == first.body
    again = parse_document(second.body)
    assert serialize_source(again) == second.body
    done = complete(edited)
    assert done.seq == 2
    assert done.content_digest == second.content_digest
    shown = preview(edited)
    assert shown.evidence_kind == "digest"
    assert shown.content_digest == second.content_digest
    assert "<script" not in shown.excerpt.lower()


def test_malformed_artifact_blocks_completion() -> None:
    """Named test: malformed artifact. Format failure raises and yields no Completion."""
    good = create_from_fixture(
        "document.md", workspace_id=WORKSPACE, kind="document", title="brief", seed="malformed"
    )
    malformed_bodies = {
        "document": b"no heading, just a blob",
        "spreadsheet": b"only,one\nrow",
        "presentation": json.dumps({"schema_version": "v1", "slides": []}).encode(),
        "chat-blob": json.dumps({"role": "assistant", "content": "here is a doc"}).encode(),
        "script": b"# Title\n<script>alert(1)</script>\n",
    }
    # Jagged CSV and empty slides are format failures; chat blob is opaque.
    bad_document = edit(good, malformed_bodies["document"])
    fmt, _ = inspect(bad_document)
    assert not fmt.ok
    with pytest.raises(ArtifactError) as raised:
        complete(bad_document)
    assert raised.value.rule_id == "artifact.format"
    assert "blocked completion" in str(raised.value)
    # Previous valid version remains addressable; completion of the good record still works.
    assert complete(good).seq == 1

    jagged = create(
        workspace_id=WORKSPACE,
        kind=ArtifactKind.SPREADSHEET,
        title="jagged",
        body=malformed_bodies["spreadsheet"],
        seed="jagged",
    )
    with pytest.raises(ArtifactError) as raised:
        complete(jagged)
    assert raised.value.rule_id == "artifact.format"

    empty_deck = create(
        workspace_id=WORKSPACE,
        kind=ArtifactKind.PRESENTATION,
        title="empty",
        body=malformed_bodies["presentation"],
        seed="empty-deck",
    )
    with pytest.raises(ArtifactError) as raised:
        complete(empty_deck)
    assert raised.value.rule_id == "artifact.format"

    blob = create(
        workspace_id=WORKSPACE,
        kind=ArtifactKind.DOCUMENT,
        title="blob",
        body=malformed_bodies["chat-blob"],
        seed="blob",
    )
    with pytest.raises(ArtifactError) as raised:
        complete(blob)
    assert raised.value.rule_id == "artifact.opaque_blob"

    scripted = create(
        workspace_id=WORKSPACE,
        kind=ArtifactKind.DOCUMENT,
        title="xss",
        body=malformed_bodies["script"],
        seed="script",
    )
    with pytest.raises(ArtifactError) as raised:
        complete(scripted)
    assert raised.value.rule_id == "artifact.format"


def test_quality_failure_also_blocks_and_skill_pack_declares_tools() -> None:
    """Quality checks run after format; the skill pack cannot invent tools."""
    heading_only = create(
        workspace_id=WORKSPACE,
        kind=ArtifactKind.DOCUMENT,
        title="empty-body",
        body=b"# Title\n",
        seed="empty-body",
    )
    fmt = validate_format(ArtifactKind.DOCUMENT, heading_only.current.body, "text/markdown")
    assert fmt.ok
    with pytest.raises(ArtifactError) as raised:
        complete(heading_only)
    assert raised.value.rule_id == "artifact.quality"

    manifest = load_skill_manifest()
    assert "artifact.create" in manifest.tool_needs
    assert "artifact.update" in manifest.tool_needs
    assert skill_pack_root() == ROOT / "packs" / "skills" / "artifacts"
    assert (skill_pack_root() / "fixtures" / "document.md").is_file()

    version = SkillVersionRef.build(
        skill_id="skl_artifacts",
        version_id="sklv_artifacts_1",
        skill_name="artifacts",
        semver="1.0.0",
        status=SkillStatus.ACTIVE,
        manifest={
            "instructions": manifest.instructions,
            "examples": list(manifest.examples),
            "tool_needs": list(manifest.tool_needs),
            "capability_needs": list(manifest.capability_needs),
            "eval_suite_id": manifest.eval_suite_id,
            "compatibility": list(manifest.compatibility),
            "recovery_guidance": manifest.recovery_guidance,
        },
    )
    allowed = resolve(
        task_text="write a document then edit the spreadsheet",
        snapshot=CapabilitySnapshot(
            tool_names=frozenset({"artifact.create", "artifact.update"}),
            capability_needs=frozenset({"artifact.write"}),
        ),
        candidates=[version],
    )
    assert allowed.resolved_ids() == ("sklv_artifacts_1",)
    denied = resolve(
        task_text="write a document",
        snapshot=CapabilitySnapshot(tool_names=frozenset({"fs.read"}), capability_needs=frozenset()),
        candidates=[version],
    )
    assert denied.resolved == ()
    assert any(item.rule_id == "skill.tool_not_available" for item in denied.excluded)


def test_spreadsheet_and_slides_parsers_are_closed() -> None:
    parsed_sheet = parse_spreadsheet((ROOT / "packs/skills/artifacts/fixtures/spreadsheet.csv").read_bytes())
    assert parsed_sheet.headers == ("metric", "value", "unit")
    assert len(parsed_sheet.rows) == 3
    parsed_deck = parse_presentation((ROOT / "packs/skills/artifacts/fixtures/slides.json").read_bytes())
    assert len(parsed_deck.slides) == 2
    assert parsed_deck.slides[0].title == "Artifact workflows"
    # Unknown keys fail closed.
    with pytest.raises(ArtifactError) as raised:
        parse_presentation(b'{"schema_version":"v1","slides":[{"title":"A","body":"b","inject":1}]}')
    assert raised.value.rule_id == "artifact.format"
    assert digest_of(parsed_sheet.canonical_bytes()) == digest_of(serialize_source(parsed_sheet))
