"""Canonical rendered-message content and its provider encodings.

`RenderedMessage.content_json` is rendered by the Context plane (INT-005). Its canonical form
is either a JSON string (plain text) or an object:

    {"parts": [{"kind": "text", "text": "..."},
               {"kind": "image", "media_type": "image/png", "data_base64": "..."},
               {"kind": "image", "url": "https://..."},
               {"kind": "document", "media_type": "application/pdf", "data_base64": "..."}],
     "tool_call_id": "...",   # tool messages only
     "tool_name": "..."}

Anything else fails closed with `VALIDATION_SCHEMA`: the gateway never guesses at content it
cannot normalize, and never silently drops a part (a dropped image or document would change
what the model sees).
"""

from __future__ import annotations

import base64
import binascii
import json
from dataclasses import dataclass
from typing import Any

from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

PART_TEXT = "text"
PART_IMAGE = "image"
PART_DOCUMENT = "document"

_ALLOWED_PART_KINDS = frozenset({PART_TEXT, PART_IMAGE, PART_DOCUMENT})
_ALLOWED_CONTENT_KEYS = frozenset({"parts", "tool_call_id", "tool_name"})
_DOCUMENT_MEDIA_TYPES = frozenset({"application/pdf"})


@dataclass(frozen=True, slots=True)
class ContentPart:
    """One normalized content part."""

    kind: str
    text: str = ""
    media_type: str = ""
    data_base64: str = ""
    url: str = ""


@dataclass(frozen=True, slots=True)
class RenderedContent:
    """Normalized content of one rendered message."""

    parts: tuple[ContentPart, ...] = ()
    tool_call_id: str = ""
    tool_name: str = ""

    @property
    def has_document(self) -> bool:
        return any(part.kind == PART_DOCUMENT for part in self.parts)

    @property
    def has_image(self) -> bool:
        return any(part.kind == PART_IMAGE for part in self.parts)

    def text(self) -> str:
        return "\n".join(part.text for part in self.parts if part.kind == PART_TEXT and part.text)


def parse_content(content_json: str, *, is_tool_message: bool) -> RenderedContent:
    """Normalize `RenderedMessage.content_json`, failing closed on any unknown shape."""
    raw = content_json.strip()
    if not raw:
        return RenderedContent()
    try:
        decoded: Any = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA, "rendered content is not valid JSON"
        ) from error
    if isinstance(decoded, str):
        return RenderedContent(parts=(ContentPart(kind=PART_TEXT, text=decoded),))
    if not isinstance(decoded, dict):
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            "rendered content must be a JSON string or an object with `parts`",
        )
    unknown = sorted(set(decoded) - _ALLOWED_CONTENT_KEYS)
    if unknown:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA, f"rendered content has unknown key(s) {unknown}"
        )
    raw_parts = decoded.get("parts", [])
    if not isinstance(raw_parts, list):
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "rendered content `parts` must be a list")
    parts = tuple(_parse_part(part) for part in raw_parts)
    tool_call_id = _opt_text(decoded.get("tool_call_id"))
    tool_name = _opt_text(decoded.get("tool_name"))
    if is_tool_message and not tool_call_id:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "a tool message must carry `tool_call_id`")
    if not is_tool_message and tool_call_id:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "only a tool message may carry `tool_call_id`")
    return RenderedContent(parts=parts, tool_call_id=tool_call_id, tool_name=tool_name)


def to_anthropic_blocks(content: RenderedContent) -> list[dict[str, object]]:
    """Anthropic Messages content blocks (DOMAIN.md §11.1 multimodal normalization)."""
    blocks: list[dict[str, object]] = []
    for part in content.parts:
        if part.kind == PART_TEXT:
            blocks.append({"type": "text", "text": part.text})
        elif part.kind == PART_IMAGE:
            if part.url:
                blocks.append({"type": "image", "source": {"type": "url", "url": part.url}})
            else:
                blocks.append(
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": part.media_type,
                            "data": part.data_base64,
                        },
                    }
                )
        elif part.kind == PART_DOCUMENT:
            blocks.append(
                {
                    "type": "document",
                    "source": {
                        "type": "base64",
                        "media_type": part.media_type,
                        "data": part.data_base64,
                    },
                }
            )
    return blocks


def to_openai_blocks(content: RenderedContent, *, allow_documents: bool) -> list[dict[str, object]]:
    """OpenAI chat-completions content parts; documents fail closed unless declared."""
    if content.has_document and not allow_documents:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            "the selected model does not declare the `documents` capability",
        )
    blocks: list[dict[str, object]] = []
    for part in content.parts:
        if part.kind == PART_TEXT:
            blocks.append({"type": "text", "text": part.text})
        elif part.kind == PART_IMAGE:
            if part.url:
                blocks.append({"type": "image_url", "image_url": {"url": part.url}})
            else:
                data_url = f"data:{part.media_type};base64,{part.data_base64}"
                blocks.append({"type": "image_url", "image_url": {"url": data_url}})
        elif part.kind == PART_DOCUMENT:
            data_url = f"data:{part.media_type};base64,{part.data_base64}"
            blocks.append({"type": "file", "file": {"file_data": data_url}})
    return blocks


def _parse_part(raw: object) -> ContentPart:
    if not isinstance(raw, dict):
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "a content part must be an object")
    kind = raw.get("kind")
    if kind not in _ALLOWED_PART_KINDS:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"unsupported content part kind {kind!r}")
    if kind == PART_TEXT:
        text = raw.get("text")
        if not isinstance(text, str) or not text:
            raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "a text part requires `text`")
        return ContentPart(kind=PART_TEXT, text=text)
    media_type = _opt_text(raw.get("media_type"))
    data_base64 = _opt_text(raw.get("data_base64"))
    url = _opt_text(raw.get("url"))
    if kind == PART_IMAGE:
        if not media_type.startswith("image/"):
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA, "an image part requires an image/* media_type"
            )
    elif media_type not in _DOCUMENT_MEDIA_TYPES:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"a document part requires media_type in {sorted(_DOCUMENT_MEDIA_TYPES)}",
        )
    if kind == PART_DOCUMENT and not data_base64:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "a document part requires base64 data")
    if not data_base64 and not url:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA, "an image part requires `data_base64` or `url`"
        )
    if data_base64:
        try:
            base64.b64decode(data_base64, validate=True)
        except (ValueError, binascii.Error) as error:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA, "content part `data_base64` is not valid base64"
            ) from error
    return ContentPart(kind=kind, media_type=media_type, data_base64=data_base64, url=url)


def _opt_text(value: object) -> str:
    return value if isinstance(value, str) else ""
