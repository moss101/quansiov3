"""Live speech-to-text (ADP-006, DOMAIN.md §10.4 E) — the real boundary for transcription.

Environment variables this module reads (it never logs their values):

  * `QUANSIO_TEST_STT_API_KEY` — a real provider credential for the transcription route. Without it
    the suite skips with an explicit `BLOCKED_EXTERNAL` marker and the task cannot be marked PASS
    on the strength of the loopback tests in `test_transcription_outputs.py`.
  * `QUANSIO_TEST_STT_AUDIO` — the path of a short real recording of speech (WAV, MP3, M4A, WebM, OGG
    or FLAC; at most 25 MiB).
  * `QUANSIO_TEST_STT_EXPECT` — a phrase the recording says; the transcript must contain it,
    compared case-insensitively on its letters and digits.
  * `QUANSIO_TEST_STT_BASE_URL` — optional: another server that speaks the provider's wire format.
    Absent, the route runs against the committed catalog's own endpoint and model.

What it proves: the gateway sends the recording to the route the catalog names, with the credential
the gateway materialized, and the answer comes back as ordered, timestamped segments that the
authority's transcript rules accept.
"""

from __future__ import annotations

import json
import os
import re
from pathlib import Path

import pytest
import yaml

from intelligence.artifacts.outputs.transcripts import AudioSource, TranscriptionJobs, build_transcript
from intelligence.model_gateway.catalog import ModelCatalog
from intelligence.model_gateway.gateway import ModelGateway

ROOT = Path(__file__).resolve().parents[3]
KEY = "QUANSIO_TEST_STT_API_KEY"
AUDIO = "QUANSIO_TEST_STT_AUDIO"
EXPECT = "QUANSIO_TEST_STT_EXPECT"
BASE_URL = "QUANSIO_TEST_STT_BASE_URL"
MEDIA_TYPES = {
    ".wav": "audio/wav",
    ".mp3": "audio/mpeg",
    ".m4a": "audio/mp4",
    ".webm": "audio/webm",
    ".ogg": "audio/ogg",
    ".flac": "audio/flac",
}


def _blocked(*names: str) -> None:
    missing = [name for name in names if not os.environ.get(name, "").strip()]
    if missing:
        pytest.skip(
            f"BLOCKED_EXTERNAL: {', '.join(missing)} not set; the live transcription suite did not run"
        )


def _gateway() -> ModelGateway:
    raw = yaml.safe_load((ROOT / "config" / "models.yaml").read_text(encoding="utf-8"))
    override = os.environ.get(BASE_URL, "").strip()
    if override:
        raw["providers"]["openai"]["base_url"] = override
    return ModelGateway(
        catalog=ModelCatalog.from_mapping(raw), environ={"QUANSIO_SECRET_PROVIDER_OPENAI": os.environ[KEY]}
    )


def _letters(text: str) -> str:
    return re.sub(r"[^a-z0-9]", "", text.lower())


def test_a_real_recording_is_transcribed_through_the_gateway() -> None:
    _blocked(KEY, AUDIO, EXPECT)
    path = Path(os.environ[AUDIO])
    audio = path.read_bytes()
    media_type = MEDIA_TYPES[path.suffix.lower()]
    outcome = _gateway().transcribe(audio, media_type, max_audio_bytes=25 * 1024 * 1024, timeout_ms=120_000)
    assert outcome.transcript.segments, "the provider returned no speech"
    document = json.loads(
        build_transcript(
            outcome.transcript,
            AudioSource("art_01J8Z3K6F1N8VQ2X5W9YADP006", "artv_01J8Z3K6F1N8VQ2X5W9YADP006", "a" * 64),
        )
    )
    assert document["trust"] == "UNTRUSTED_EXTERNAL"
    said = _letters(" ".join(segment["text"] for segment in document["segments"]))
    assert _letters(os.environ[EXPECT]) in said, "the recording's own words are in the transcript"
    starts = [segment["start_ms"] for segment in document["segments"]]
    assert starts == sorted(starts) and starts[0] >= 0


def test_the_submit_and_poll_job_finishes_a_real_transcription_once() -> None:
    _blocked(KEY, AUDIO, EXPECT)
    path = Path(os.environ[AUDIO])
    jobs = TranscriptionJobs(_gateway())
    try:
        source = AudioSource("art_01J8Z3K6F1N8VQ2X5W9YADP006", "artv_01J8Z3K6F1N8VQ2X5W9YADP006", "a" * 64)
        status = jobs.request(
            "live-request-0001", path.read_bytes(), MEDIA_TYPES[path.suffix.lower()], source, wait_ms=30_000
        )
        while status.state == "pending":
            status = jobs.request(
                "live-request-0001",
                path.read_bytes(),
                MEDIA_TYPES[path.suffix.lower()],
                source,
                wait_ms=30_000,
            )
        assert status.state == "done", status
        assert status.transcript is not None
        spoken = " ".join(segment["text"] for segment in json.loads(status.transcript)["segments"])
        assert _letters(os.environ[EXPECT]) in _letters(spoken)
    finally:
        jobs.close()
