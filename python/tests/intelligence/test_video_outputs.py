"""Video operations and calls (ADP-006, DOMAIN.md §10.4 D): what the plane proposes, the registry accepts."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import yaml
from jsonschema import Draft202012Validator

from intelligence.artifacts import ArtifactError
from intelligence.artifacts.outputs import video

ROOT = Path(__file__).resolve().parents[3]


def _declaration(tool: str) -> dict:
    registry = yaml.safe_load((ROOT / "config" / "tools.yaml").read_text(encoding="utf-8"))
    return next(item for item in registry["tools"] if item["name"] == tool)


def _validator(tool: str) -> Draft202012Validator:
    return Draft202012Validator(_declaration(tool)["input_schema"])


def test_the_operation_table_is_the_registrys_enum():
    declared = _declaration("video.edit")["input_schema"]["properties"]["ops"]["items"]["properties"]["op"][
        "enum"
    ]
    assert sorted(declared) == sorted(video.OP_FIELDS)
    # Every field an operation can take is a property of the registry's operation object.
    properties = set(_declaration("video.edit")["input_schema"]["properties"]["ops"]["items"]["properties"])
    for name, (required, optional) in video.OP_FIELDS.items():
        assert set(required) | set(optional) <= properties, name


def test_a_new_timeline_starts_at_revision_one_and_is_bounded():
    source = json.loads(video.empty_timeline(640, 360, 25))
    assert source["revision"] == 1 and source["output"] == {"width": 640, "height": 360, "fps": 25}
    assert source["media"] == [] and source["tracks"] == [] and source["subtitles"] == []
    for bad in [(641, 360, 25), (8, 360, 25), (640, 360, 0), (640, 360, 1000), (99999, 360, 25)]:
        with pytest.raises(ArtifactError):
            video.empty_timeline(*bad)


def test_an_operation_takes_exactly_the_fields_its_kind_takes():
    assert video.op("add_track", id="v1", kind="video") == {"op": "add_track", "id": "v1", "kind": "video"}
    assert video.op("trim_clip", clip_id="c1", out_ms=5000) == {
        "op": "trim_clip",
        "clip_id": "c1",
        "out_ms": 5000,
    }
    assert video.op("move_clip", clip_id="c1", start_ms=0, track_id=None) == {
        "op": "move_clip",
        "clip_id": "c1",
        "start_ms": 0,
    }
    for name, fields in [
        ("frobnicate", {}),
        ("add_track", {"id": "v1"}),
        ("add_track", {"id": "v1", "kind": "video", "clips": []}),
        ("trim_clip", {"clip_id": "c1"}),
        ("set_output", {}),
        ("remove_media", {"id": "m", "duration_ms": 1}),
    ]:
        with pytest.raises(ArtifactError):
            video.op(name, **fields)


def test_every_call_is_what_the_registry_accepts():
    ops = [
        video.op(
            "add_media", id="talk", artifact_id="art_V", kind="video", duration_ms=20000, has_audio=True
        ),
        video.op("add_track", id="v1", kind="video"),
        video.op(
            "add_clips",
            track_id="v1",
            clips=[{"id": "c1", "media_id": "talk", "start_ms": 0, "in_ms": 0, "out_ms": 5000}],
        ),
        video.op(
            "set_keyframes",
            clip_id="c1",
            keyframes=[
                {"at_ms": 0, "property": "volume", "value": 0.0},
                {"at_ms": 1000, "property": "volume", "value": 1.0},
            ],
        ),
        video.op("add_subtitles", subtitles=[{"id": "s1", "start_ms": 0, "end_ms": 900, "text": "Hi"}]),
        video.op("subtitles_from_transcript", transcript_artifact_id="art_T", id_prefix="t", offset_ms=250),
        video.op("split_clip", clip_id="c1", at_ms=2000, new_clip_id="c1b"),
        video.op("set_output", width=1280, height=720),
    ]
    for call in [
        video.edit_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", 1, ops),
        video.edit_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", 3, ops[:2], dry_run=True),
        video.render_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", 3),
        video.inspect_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", [0, 1500, 3000]),
    ]:
        errors = sorted(_validator(call.tool).iter_errors(dict(call.arguments)), key=str)
        assert not errors, (call.tool, [error.message for error in errors][:3])


def test_a_batch_is_bounded_before_it_is_proposed():
    one = video.op("add_track", id="v1", kind="video")
    with pytest.raises(ArtifactError):
        video.edit_call("art_x", 1, [])
    with pytest.raises(ArtifactError):
        video.edit_call("art_x", 1, [one] * 65)
    with pytest.raises(ArtifactError):
        video.edit_call("art_x", 0, [one])
    with pytest.raises(ArtifactError):
        video.inspect_call("art_x", [])
    with pytest.raises(ArtifactError):
        video.inspect_call("art_x", list(range(25)))


def test_the_registry_refuses_what_the_plane_would_never_send():
    call = video.edit_call("art_x", 1, [video.op("add_track", id="v1", kind="video")])
    arguments = dict(call.arguments)
    assert not list(_validator("video.edit").iter_errors(arguments))
    assert list(_validator("video.edit").iter_errors({**arguments, "force": True}))
    ops = [dict(item) for item in arguments["ops"]]
    ops[0]["surprise"] = 1
    assert list(_validator("video.edit").iter_errors({**arguments, "ops": ops}))
    assert list(
        _validator("video.edit").iter_errors({**arguments, "ops": [{"op": "add_track", "id": "v"}] * 65})
    )
