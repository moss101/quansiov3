"""Transcripts and the submit-and-poll job behind `audio.transcribe` (ADP-006, DOMAIN.md §10.4 E).

Two things live here and neither commits anything:

* :func:`build_transcript` turns what a provider said into the closed transcript document the authority
  stores (`application/vnd.quansio.transcript+json`): segments in order with their times, labelled
  `UNTRUSTED_EXTERNAL`, bound to the audio it is of. The same rules the authority applies are applied
  first, so a provider answer that cannot be a transcript is an error here and not a surprise there.
* :class:`TranscriptionJobs` is the submit-and-poll state of long transcriptions. A request id names a
  job: the first call submits it to the gateway on a worker thread, a later call with the same id reports
  where it is, and the same id with different audio or language is a conflict, never a second job. Nothing
  here is durable: a restart forgets the jobs, the authority's own store of finished transcripts is what
  makes a request id idempotent across restarts, and a lost job is submitted again (a transcription reads
  audio and changes nothing outside the workspace).
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import threading
import time
from collections.abc import Callable
from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import dataclass
from typing import Literal

from intelligence.artifacts.models import ArtifactError
from intelligence.artifacts.outputs.limits import shipped_config
from intelligence.model_gateway.errors import GatewayError
from intelligence.model_gateway.gateway import ModelGateway
from intelligence.model_gateway.transcription import ProviderTranscript

MEDIA_TYPE = "application/vnd.quansio.transcript+json"
TRUST = "UNTRUSTED_EXTERNAL"
RULE_SHAPE = "output.transcript.shape"
RULE_REQUEST = "output.transcript.request"
MAX_SEGMENT_CHARS = 2000


@dataclass(frozen=True, slots=True)
class AudioSource:
    """The audio a transcript is of."""

    artifact_id: str
    version_id: str
    content_digest: str


def build_transcript(provider: ProviderTranscript, source: AudioSource) -> bytes:
    """The transcript document for a provider's answer, checked by the authority's rules."""
    config = shipped_config().transcription
    slack = int(config["segment_overlap_ms"])
    segments: list[dict[str, object]] = []
    previous_end: int | None = None
    for position, segment in enumerate(provider.segments):
        if segment.start_ms >= segment.end_ms or segment.end_ms > provider.duration_ms:
            raise ArtifactError(
                "VALIDATION_SCHEMA",
                RULE_SHAPE,
                f"segment {position} runs {segment.start_ms}..{segment.end_ms} ms in a recording of "
                f"{provider.duration_ms} ms",
            )
        if previous_end is not None and segment.start_ms + slack < previous_end:
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_SHAPE, f"segment {position} starts before the one before it ends"
            )
        previous_end = segment.end_ms
        if len(segment.text) > MAX_SEGMENT_CHARS or any(
            (ord(c) < 32 and c not in "\n\t") or ord(c) == 127 for c in segment.text
        ):
            raise ArtifactError(
                "VALIDATION_SCHEMA",
                RULE_SHAPE,
                f"segment {position} is not 1 to {MAX_SEGMENT_CHARS} plain characters",
            )
        segments.append(
            {"index": position, "start_ms": segment.start_ms, "end_ms": segment.end_ms, "text": segment.text}
        )
    if len(segments) > int(config["max_segments"]):
        raise ArtifactError(
            "VALIDATION_BOUNDS", RULE_SHAPE, f"a transcript has at most {config['max_segments']} segments"
        )
    document = {
        "schema_version": "v1",
        "trust": TRUST,
        "source": {
            "artifact_id": source.artifact_id,
            "version_id": source.version_id,
            "content_digest": source.content_digest,
        },
        "language": provider.language,
        "duration_ms": provider.duration_ms,
        "segments": segments,
    }
    return json.dumps(document, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


JobState = Literal["pending", "done", "failed"]


@dataclass(frozen=True, slots=True)
class JobStatus:
    """Where a transcription job is."""

    state: JobState
    job_id: str
    poll_after_ms: int = 0
    transcript: bytes | None = None
    error_code: str = ""
    error_message: str = ""
    retryable: bool = False
    model: str = ""
    provider: str = ""


@dataclass(slots=True)
class _Job:
    fingerprint: str
    created_ms: int
    future: Future[tuple[bytes, str, str]]


class TranscriptionJobs:
    """Submit and poll, idempotent by request id (§10.4 E)."""

    def __init__(
        self,
        gateway: ModelGateway,
        *,
        clock: Callable[[], float] = time.time,
        workers: int = 2,
        dlp_profile: str = "",
    ) -> None:
        self._gateway = gateway
        self._clock = clock
        self._dlp_profile = dlp_profile
        self._lock = threading.Lock()
        self._jobs: dict[str, _Job] = {}
        self._pool = ThreadPoolExecutor(max_workers=workers, thread_name_prefix="quansio-transcribe")
        self._config = shipped_config().transcription

    def close(self) -> None:
        """Stop accepting work and wait for what is running."""
        self._pool.shutdown(wait=True, cancel_futures=True)

    def _now_ms(self) -> int:
        return int(self._clock() * 1000)

    def _job_id(self, request_id: str) -> str:
        return f"tj_{hashlib.sha256(request_id.encode()).hexdigest()[:26]}"

    def request(
        self,
        request_id: str,
        audio: bytes,
        media_type: str,
        source: AudioSource,
        *,
        language: str | None = None,
        wait_ms: int = 0,
    ) -> JobStatus:
        """Submit the job if this request id is new, then report where it is, waiting up to `wait_ms`."""
        if not 8 <= len(request_id) <= 64 or not all(c.isalnum() or c in "-_" for c in request_id):
            raise ArtifactError(
                "VALIDATION_SCHEMA", RULE_REQUEST, "request_id is 8 to 64 letters, digits, '-' or '_'"
            )
        fingerprint = hashlib.sha256(
            audio + b"\0" + media_type.encode() + b"\0" + (language or "").encode()
        ).hexdigest()
        job_id = self._job_id(request_id)
        with self._lock:
            self._expire()
            job = self._jobs.get(request_id)
            if job is not None and job.fingerprint != fingerprint:
                raise ArtifactError(
                    "CONFLICT_STATE",
                    RULE_REQUEST,
                    f"request {request_id} is already a transcription of other audio or another language",
                )
            if job is None:
                job = _Job(
                    fingerprint=fingerprint,
                    created_ms=self._now_ms(),
                    future=self._pool.submit(self._run, audio, media_type, source, language),
                )
                self._jobs[request_id] = job
        wait = min(max(wait_ms, 0), int(self._config["wait_max_ms"]))
        if wait:
            with contextlib.suppress(Exception):  # the outcome is read below, whatever it was
                job.future.result(timeout=wait / 1000.0)
        return self._status(request_id, job, job_id)

    def _run(
        self, audio: bytes, media_type: str, source: AudioSource, language: str | None
    ) -> tuple[bytes, str, str]:
        outcome = self._gateway.transcribe(
            audio,
            media_type,
            language=language,
            max_audio_bytes=int(self._config["max_audio_bytes"]),
            dlp_profile=self._dlp_profile,
        )
        return build_transcript(outcome.transcript, source), outcome.model_catalog_id, outcome.provider

    def _status(self, request_id: str, job: _Job, job_id: str) -> JobStatus:
        if not job.future.done():
            return JobStatus("pending", job_id, poll_after_ms=int(self._config["poll_after_ms"]))
        error = job.future.exception()
        if error is None:
            document, model, provider = job.future.result()
            return JobStatus("done", job_id, transcript=document, model=model, provider=provider)
        # A failure is reported once and the id is free again: whether to try again is the caller's call.
        with self._lock:
            self._jobs.pop(request_id, None)
        if isinstance(error, GatewayError):
            return JobStatus(
                "failed",
                job_id,
                error_code=str(error.code),
                error_message=str(error),
                retryable=error.retryable,
            )
        if isinstance(error, ArtifactError):
            return JobStatus("failed", job_id, error_code=error.code, error_message=error.detail)
        return JobStatus("failed", job_id, error_code="INTERNAL", error_message=type(error).__name__)

    def _expire(self) -> None:
        horizon = self._now_ms() - int(self._config["job_ttl_ms"])
        for request_id in [
            key for key, job in self._jobs.items() if job.created_ms < horizon and job.future.done()
        ]:
            del self._jobs[request_id]
