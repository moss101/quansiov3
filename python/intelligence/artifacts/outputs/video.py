"""Video: the timeline source, typed edit operations and their calls (ADP-006, DOMAIN.md §10.4 D).

The plane does not edit a timeline; it **proposes a batch**. This module builds the operations (each one
takes exactly the fields its kind takes, and a field that is missing or misplaced is an error here, as it
is in the authority) and the governed calls: `video.edit` against an exact revision with an optional dry
run, `video.render` and `video.inspect`. The authority applies a batch atomically to a copy, validates the
timeline and writes a version, or refuses the whole batch.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from typing import Any

from intelligence.artifacts.models import ArtifactError
from intelligence.artifacts.outputs.calls import OutputCall
from intelligence.artifacts.outputs.limits import shipped_config

MEDIA_TYPE = "application/vnd.quansio.video-timeline+json"
RULE = "output.video.op"

#: What each operation takes: (required, optional). The authority holds the same table (``ops::fields_of``).
OP_FIELDS: Mapping[str, tuple[tuple[str, ...], tuple[str, ...]]] = {
    "add_media": (("id", "artifact_id", "kind", "duration_ms"), ("has_audio",)),
    "remove_media": (("id",), ()),
    "add_track": (("id", "kind"), ()),
    "add_clips": (("track_id", "clips"), ()),
    "trim_clip": (("clip_id",), ("in_ms", "out_ms")),
    "split_clip": (("clip_id", "at_ms", "new_clip_id"), ()),
    "move_clip": (("clip_id", "start_ms"), ("track_id",)),
    "remove_clips": (("clip_ids",), ()),
    "set_keyframes": (("clip_id", "keyframes"), ()),
    "set_volume": (("clip_id", "volume"), ()),
    "add_subtitles": (("subtitles",), ()),
    "remove_subtitles": (("ids",), ()),
    "subtitles_from_transcript": (("transcript_artifact_id", "id_prefix"), ("offset_ms",)),
    "set_output": ((), ("width", "height", "fps")),
}


def empty_timeline(width: int = 1280, height: int = 720, fps: int = 30) -> bytes:
    """A new timeline at revision 1: what `artifact.create` makes (later revisions are `video.edit`'s)."""
    limits = shipped_config().video
    if (
        width % 2
        or height % 2
        or not 16 <= width <= int(limits["max_dimension"])
        or not 16 <= height <= int(limits["max_dimension"])
    ):
        raise ArtifactError("VALIDATION_BOUNDS", RULE, "each side of the output is an even number from 16 up")
    if not 1 <= fps <= int(limits["max_fps"]):
        raise ArtifactError("VALIDATION_BOUNDS", RULE, f"fps is 1 to {limits['max_fps']}")
    payload = {
        "schema_version": "v1",
        "revision": 1,
        "output": {"width": width, "height": height, "fps": fps},
        "media": [],
        "tracks": [],
        "subtitles": [],
    }
    return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")


def op(name: str, **fields: Any) -> dict[str, Any]:
    """One operation, with the fields its kind takes and no others."""
    if name not in OP_FIELDS:
        raise ArtifactError("VALIDATION_UNKNOWN_FIELD", RULE, f"{name!r} is not an operation")
    required, optional = OP_FIELDS[name]
    given = {key: value for key, value in fields.items() if value is not None}
    extra = sorted(set(given) - set(required) - set(optional))
    if extra:
        raise ArtifactError("VALIDATION_UNKNOWN_FIELD", RULE, f"{name} does not take {', '.join(extra)}")
    missing = [key for key in required if key not in given]
    if missing:
        raise ArtifactError("VALIDATION_SCHEMA", RULE, f"{name} needs {', '.join(missing)}")
    if name == "trim_clip" and "in_ms" not in given and "out_ms" not in given:
        raise ArtifactError("VALIDATION_SCHEMA", RULE, "trim_clip needs in_ms or out_ms")
    if name == "set_output" and not given:
        raise ArtifactError("VALIDATION_SCHEMA", RULE, "set_output needs width, height or fps")
    return {"op": name, **given}


def edit_call(
    artifact_id: str, expected_revision: int, ops: Sequence[Mapping[str, Any]], *, dry_run: bool = False
) -> OutputCall:
    """`video.edit`: a batch against the revision the plane read."""
    limit = int(shipped_config().video["max_ops_per_batch"])
    if not ops or len(ops) > limit:
        raise ArtifactError("VALIDATION_BOUNDS", RULE, f"a batch is 1 to {limit} operations")
    if expected_revision < 1:
        raise ArtifactError("VALIDATION_BOUNDS", RULE, "a revision starts at 1")
    return OutputCall(
        "video.edit",
        {
            "artifact_id": artifact_id,
            "expected_revision": expected_revision,
            "dry_run": dry_run,
            "ops": [dict(item) for item in ops],
        },
    )


def render_call(artifact_id: str, expected_revision: int) -> OutputCall:
    """`video.render`: the timeline at exactly this revision, as a supervised job."""
    return OutputCall("video.render", {"artifact_id": artifact_id, "expected_revision": expected_revision})


def inspect_call(artifact_id: str, times_ms: Sequence[int]) -> OutputCall:
    """`video.inspect`: frames of a video at chosen times, as a contact sheet."""
    limit = int(shipped_config().video["inspect"]["max_frames"])
    if not times_ms or len(times_ms) > limit:
        raise ArtifactError("VALIDATION_BOUNDS", RULE, f"an inspection looks at 1 to {limit} times")
    return OutputCall("video.inspect", {"artifact_id": artifact_id, "times_ms": list(times_ms)})
