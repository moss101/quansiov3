"""Speech-to-text through the one gateway (ADP-006, DOMAIN.md §10.4 E).

A transcription is a model call like any other: the gateway resolves the route from the catalog
(`config/models.yaml`: a model that declares the `transcription` capability), enforces the data policy
before a byte is built, materializes the provider credential at the moment the request is signed and
sends one bounded request over the pinned transport. Nothing else holds the credential: not the worker,
not the Rust runtime, not the model. The provider's answer is **data**: it is decoded strictly, bounded,
and handed on labelled `UNTRUSTED_EXTERNAL` by the caller; nothing in it is read as an instruction.

Only the wire is specific to a provider family. OpenAI's `audio/transcriptions` takes a multipart form;
the OpenAI-compatible profile takes the same form relative to a service's own base URL. Anthropic
publishes no transcription endpoint, so a route that resolves there is a routing error, not a chat call
made to look like one.
"""

from __future__ import annotations

import hashlib
import json
import math
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Protocol

from intelligence.model_gateway.adapters.base import PreparedRequest
from intelligence.model_gateway.catalog import ProviderKind
from intelligence.model_gateway.credentials import SecretValue
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

#: The capability a catalog model declares to be a transcription route (config/outputs.yaml names it too).
CAPABILITY = "transcription"

RULE_WIRE = "transcription.wire"
RULE_INPUT = "transcription.input"
RULE_RESPONSE = "transcription.response"

#: Ceiling on the response bytes one transcription may read before it is refused.
RESPONSE_MAX_BYTES = 8 * 1024 * 1024

_MEDIA_TYPES: Mapping[str, str] = {
    "audio/mpeg": "mp3",
    "audio/mp3": "mp3",
    "audio/mp4": "m4a",
    "audio/x-m4a": "m4a",
    "audio/aac": "m4a",
    "audio/wav": "wav",
    "audio/x-wav": "wav",
    "audio/wave": "wav",
    "audio/webm": "webm",
    "audio/ogg": "ogg",
    "audio/flac": "flac",
}
_LANGUAGE = re.compile(r"^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})?$")


@dataclass(frozen=True, slots=True)
class Segment:
    """One stretch of speech."""

    start_ms: int
    end_ms: int
    text: str


@dataclass(frozen=True, slots=True)
class ProviderTranscript:
    """What a provider said an audio file says, decoded and bounded."""

    language: str
    duration_ms: int
    segments: tuple[Segment, ...]


@dataclass(frozen=True, slots=True)
class TranscriptionCallContext:
    """Everything a wire may use to build one provider request."""

    model_catalog_id: str
    wire_model_id: str
    provider: str
    credential: SecretValue
    base_url: str
    audio: bytes
    media_type: str
    language: str | None
    timeout_seconds: float


class TranscriptionWire(Protocol):
    """One provider family's transcription wire format."""

    kind: ProviderKind

    def build_request(self, context: TranscriptionCallContext) -> PreparedRequest: ...

    def decode(self, prepared: PreparedRequest, body: bytes) -> ProviderTranscript: ...


def validate_input(audio: bytes, media_type: str, language: str | None, *, max_bytes: int) -> str:
    """The checks every transcription request passes before a byte is built; the file extension to send."""
    if not audio:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"no audio to transcribe (rule {RULE_INPUT})")
    if len(audio) > max_bytes:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_BOUNDS,
            f"the audio is {len(audio)} bytes, over the {max_bytes}-byte limit (rule {RULE_INPUT})",
        )
    extension = _MEDIA_TYPES.get(media_type.strip().lower())
    if extension is None:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"{media_type!r} is not an audio type a transcription route accepts (rule {RULE_INPUT})",
        )
    if language is not None and not _LANGUAGE.fullmatch(language):
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"language is a short tag such as en or pt-BR (rule {RULE_INPUT})",
        )
    return extension


def multipart(
    fields: list[tuple[str, str]], file_field: str, filename: str, media_type: str, audio: bytes
) -> tuple[bytes, str]:
    """A `multipart/form-data` body and its content type. The boundary is derived from the audio so the
    same request is the same bytes, and cannot occur inside the audio by construction (it is re-derived
    until it does not)."""
    boundary = "quansio" + hashlib.sha256(audio).hexdigest()[:32]
    while boundary.encode() in audio:
        boundary = "quansio" + hashlib.sha256(boundary.encode()).hexdigest()[:32]
    parts: list[bytes] = []
    for name, value in fields:
        parts.append(
            f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode()
        )
    parts.append(
        f'--{boundary}\r\nContent-Disposition: form-data; name="{file_field}"; filename="{filename}"\r\n'
        f"Content-Type: {media_type}\r\n\r\n".encode()
        + audio
        + b"\r\n"
    )
    parts.append(f"--{boundary}--\r\n".encode())
    return b"".join(parts), f"multipart/form-data; boundary={boundary}"


class OpenAITranscriptionWire:
    """OpenAI `POST {base_url}/v1/audio/transcriptions`; the compatible profile reuses it."""

    kind = ProviderKind.OPENAI
    PATH = "/v1/audio/transcriptions"
    __slots__ = ()

    def build_request(self, context: TranscriptionCallContext) -> PreparedRequest:
        extension = _MEDIA_TYPES[context.media_type.strip().lower()]
        fields = [
            ("model", context.wire_model_id),
            ("response_format", "verbose_json"),
            ("timestamp_granularities[]", "segment"),
        ]
        if context.language:
            fields.append(("language", context.language.split("-")[0].lower()))
        body, content_type = multipart(
            fields, "file", f"audio.{extension}", context.media_type, context.audio
        )
        return PreparedRequest(
            provider=context.provider,
            provider_kind=self.kind,
            model_catalog_id=context.model_catalog_id,
            url=f"{context.base_url}{self.PATH}",
            headers={
                "content-type": content_type,
                "accept": "application/json",
                "authorization": context.credential.with_scheme("Bearer"),
                "user-agent": "quansio-intelligence-model-gateway",
            },
            body=body,
            timeout_seconds=context.timeout_seconds,
            disabled_features=(),
        )

    def decode(self, prepared: PreparedRequest, body: bytes) -> ProviderTranscript:
        return parse_response(body)


class OpenAICompatibleTranscriptionWire(OpenAITranscriptionWire):
    """The generic endpoint profile: the same form, relative to an SDK-style base URL."""

    kind = ProviderKind.OPENAI_COMPATIBLE
    PATH = "/audio/transcriptions"
    __slots__ = ()


WIRES: tuple[type[TranscriptionWire], ...] = (OpenAITranscriptionWire, OpenAICompatibleTranscriptionWire)


def wire_for_kind(kind: ProviderKind) -> TranscriptionWire:
    """The wire registered for a provider kind; a kind without one fails closed."""
    for wire_class in WIRES:
        if wire_class.kind == kind:
            return wire_class()
    raise GatewayError(
        GatewayErrorCode.ROUTE_UNAVAILABLE,
        f"provider kind {kind} has no transcription endpoint (rule {RULE_WIRE})",
    )


def _shape(detail: str) -> GatewayError:
    return GatewayError(GatewayErrorCode.PROVIDER_UNAVAILABLE, f"{detail} (rule {RULE_RESPONSE})")


def _milliseconds(value: object, what: str) -> int:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or value < 0
    ):
        raise _shape(f"{what} is not a time in seconds")
    return round(float(value) * 1000)


def parse_response(body: bytes) -> ProviderTranscript:
    """Decode a provider's `verbose_json` answer strictly: ordered segments with their times, or an error."""
    try:
        decoded = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise _shape("provider sent a malformed transcription payload") from error
    if not isinstance(decoded, dict):
        raise _shape("provider sent a non-object transcription payload")
    error_payload = decoded.get("error")
    if error_payload is not None:
        code = error_payload.get("code") if isinstance(error_payload, dict) else None
        if code in {"rate_limit_exceeded", "insufficient_quota"}:
            raise GatewayError(
                GatewayErrorCode.PROVIDER_RATE_LIMITED,
                f"provider rate limited the transcription call (rule {RULE_RESPONSE})",
                retryable=True,
            )
        raise GatewayError(
            GatewayErrorCode.PROVIDER_UNAVAILABLE,
            f"provider reported a transcription failure (rule {RULE_RESPONSE})",
        )
    duration_ms = _milliseconds(decoded.get("duration"), "duration")
    entries = decoded.get("segments")
    if not isinstance(entries, list):
        raise _shape("the payload carries no segments")
    segments: list[Segment] = []
    for position, entry in enumerate(entries):
        if not isinstance(entry, dict) or not isinstance(entry.get("text"), str):
            raise _shape(f"segment {position} is not a text segment")
        start = _milliseconds(entry.get("start"), f"segment {position} start")
        end = _milliseconds(entry.get("end"), f"segment {position} end")
        text = " ".join(entry["text"].split())
        if not text:
            continue
        segments.append(Segment(start, min(max(end, start + 1), max(duration_ms, start + 1)), text))
    language = decoded.get("language")
    tag = re.sub(r"[^a-z0-9-]", "", str(language).lower())[:12] if isinstance(language, str) else ""
    return ProviderTranscript(
        language=tag if len(tag) >= 2 else "und", duration_ms=duration_ms, segments=tuple(segments)
    )
