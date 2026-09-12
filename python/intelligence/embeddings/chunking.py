"""Deterministic chunking for the derived embedding index (INT-011, DOMAIN.md §11.4).

A chunk is the unit the vector index stores, so the chunker is where two properties are won
or lost:

* **Determinism.** The same text always yields the same chunks, in the same order, with the
  same digests — the index key includes the digest, so a non-deterministic chunker would
  re-embed unchanged content forever and make a rebuild look like a diff.
* **Stability under edits.** Chunks are packed on paragraph, then sentence, then word
  boundaries, so appending a sentence to a document leaves its earlier chunks unchanged and
  only the tail is re-embedded.

The digest is over the chunk text alone (not its position), so the same passage in two
sources is embedded once per source but keyed identically, and a rebuild can prove it.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass

#: Characters per chunk when the caller does not ask for something else.
DEFAULT_CHUNK_CHARS = 1_200
#: Characters two neighbouring chunks share, so a sentence on a boundary is still retrievable.
DEFAULT_OVERLAP_CHARS = 200
#: Below this a chunk is not worth an embedding row.
MIN_CHUNK_CHARS = 32
#: Above this the provider request is refused rather than silently truncated.
MAX_CHUNK_CHARS = 8_000

#: Refusal rules, named so a caller can tell which one fired.
RULE_PARAMETERS = "chunk.parameters"
RULE_EMPTY = "chunk.empty"
RULE_TOO_LONG = "chunk.too_long"


class ChunkingError(ValueError):
    """A chunking request that cannot mean what it says (``VALIDATION_SCHEMA``)."""

    def __init__(self, rule_id: str, detail: str) -> None:
        super().__init__(detail)
        self.code = "VALIDATION_SCHEMA"
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class Chunk:
    """One chunk of a source, with the digest the index keys on."""

    index: int
    text: str
    content_digest: str

    @property
    def chars(self) -> int:
        return len(self.text)


def content_digest(text: str) -> str:
    """The digest an index row is keyed by: sha256 over the exact chunk text."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def chunk_text(
    text: str,
    *,
    chunk_chars: int = DEFAULT_CHUNK_CHARS,
    overlap_chars: int = DEFAULT_OVERLAP_CHARS,
) -> tuple[Chunk, ...]:
    """Split ``text`` into overlapping, boundary-aligned chunks.

    Raises [`ChunkingError`] for a nonsensical window, empty content, or a single
    indivisible run of characters longer than a chunk may be.
    """
    if chunk_chars < MIN_CHUNK_CHARS or chunk_chars > MAX_CHUNK_CHARS:
        raise ChunkingError(
            RULE_PARAMETERS,
            f"chunk_chars must be between {MIN_CHUNK_CHARS} and {MAX_CHUNK_CHARS}, got {chunk_chars}",
        )
    if overlap_chars < 0 or overlap_chars >= chunk_chars:
        raise ChunkingError(
            RULE_PARAMETERS,
            f"overlap_chars must be >= 0 and < chunk_chars ({chunk_chars}), got {overlap_chars}",
        )
    body = text.strip()
    if not body:
        raise ChunkingError(RULE_EMPTY, "there is nothing to embed")

    pieces: list[str] = []
    for paragraph in body.split("\n\n"):
        stripped = paragraph.strip()
        if not stripped:
            continue
        if len(stripped) <= chunk_chars:
            pieces.append(stripped)
            continue
        pieces.extend(_split_oversized(stripped, chunk_chars))

    windows: list[str] = []
    current = ""
    for piece in pieces:
        if not current:
            current = piece
        elif len(current) + 2 + len(piece) <= chunk_chars:
            current = f"{current}\n\n{piece}"
        else:
            windows.append(current)
            tail = current[-overlap_chars:] if overlap_chars else ""
            current = f"{tail}\n\n{piece}".strip() if tail else piece
    if current:
        windows.append(current)

    chunks = [window for window in windows if window]
    if not chunks:
        raise ChunkingError(RULE_EMPTY, "there is nothing to embed")
    return tuple(
        Chunk(index=index, text=window, content_digest=content_digest(window))
        for index, window in enumerate(chunks)
    )


def _split_oversized(paragraph: str, chunk_chars: int) -> list[str]:
    """Break a paragraph that cannot fit one chunk, on sentence then word boundaries."""
    sentences = _sentences(paragraph)
    pieces: list[str] = []
    for sentence in sentences:
        if len(sentence) <= chunk_chars:
            pieces.append(sentence)
            continue
        pieces.extend(_split_words(sentence, chunk_chars))
    return pieces


def _sentences(paragraph: str) -> list[str]:
    """Split on sentence ends, keeping the punctuation with the sentence."""
    parts: list[str] = []
    current = ""
    for character in paragraph:
        current += character
        if character in ".!?" and len(current.strip()) > 1:
            parts.append(current.strip())
            current = ""
    if current.strip():
        parts.append(current.strip())
    return parts or [paragraph]


def _split_words(text: str, chunk_chars: int) -> list[str]:
    """Last resort: pack words up to the window, refusing a word that cannot fit at all."""
    words = text.split()
    pieces: list[str] = []
    current = ""
    for word in words:
        if len(word) > chunk_chars:
            raise ChunkingError(
                RULE_TOO_LONG,
                f"a single word of {len(word)} characters exceeds chunk_chars ({chunk_chars})",
            )
        if not current:
            current = word
        elif len(current) + 1 + len(word) <= chunk_chars:
            current = f"{current} {word}"
        else:
            pieces.append(current)
            current = word
    if current:
        pieces.append(current)
    return pieces
