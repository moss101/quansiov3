"""Governed tool calls for the output tools (ADP-006, DOMAIN.md §7.5, §10.4).

An adapter builds bytes; this module builds the call that proposes them. The arguments are exactly the
ones the Tool Registry's `input_schema` (``config/tools.yaml``) accepts — the tests validate every call
against that schema — and the call is what the runtime reserves, authorizes and dispatches to the
authority, which verifies the bytes before it commits a version. A call never says its output is good.
"""

from __future__ import annotations

import base64
import hashlib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from intelligence.artifacts.outputs.decks import Page, RenderedDeck

DECK_RENDER = "deck.render"
DECK_EXPORT = "deck.export"
DECK_DELIVER = "deck.deliver"


@dataclass(frozen=True, slots=True)
class OutputCall:
    """One governed tool call: the tool name and the arguments its registry schema accepts."""

    tool: str
    arguments: Mapping[str, Any]


def deck_render_call(
    artifact_id: str, rendered: RenderedDeck, pages: Sequence[Page] | None = None
) -> OutputCall:
    """`deck.render`: the shared stylesheet and the pages to lay out (all, unless a subset is given)."""
    chosen = rendered.pages if pages is None else tuple(pages)
    return OutputCall(
        DECK_RENDER,
        {
            "artifact_id": artifact_id,
            "css": rendered.css,
            "pages": [{"index": page.index, "fragment": page.fragment} for page in chosen],
        },
    )


def deck_export_pdf_call(artifact_id: str) -> OutputCall:
    """`deck.export` as PDF: the authority prints the pages it has already verified."""
    return OutputCall(DECK_EXPORT, {"artifact_id": artifact_id, "format": "pdf"})


def deck_export_pptx_call(artifact_id: str, package: bytes) -> OutputCall:
    """`deck.export` as PPTX: the package the plane wrote, bound to its digest."""
    return OutputCall(
        DECK_EXPORT,
        {
            "artifact_id": artifact_id,
            "format": "pptx",
            "content_base64": base64.b64encode(package).decode("ascii"),
            "content_digest": hashlib.sha256(package).hexdigest(),
        },
    )


def deck_deliver_call(artifact_id: str) -> OutputCall:
    """`deck.deliver`: the completion gate."""
    return OutputCall(DECK_DELIVER, {"artifact_id": artifact_id})


DIAGRAM_RENDER = "diagram.render"
DOCUMENT_EXPORT_PDF = "document.export_pdf"


def diagram_render_call(artifact_id: str, svg: str) -> OutputCall:
    """`diagram.render`: the SVG laid out for a diagram source, to be checked against that source."""
    return OutputCall(DIAGRAM_RENDER, {"artifact_id": artifact_id, "svg": svg})


def document_export_pdf_call(artifact_id: str, html: str) -> OutputCall:
    """`document.export_pdf`: the HTML body rendered from a document's Markdown, to be printed."""
    return OutputCall(DOCUMENT_EXPORT_PDF, {"artifact_id": artifact_id, "html": html})
