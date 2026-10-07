"""Output adapters (ADP-006, DOMAIN.md §10.4): deterministic generators for decks, printed documents,
diagrams, video edits, transcripts and the web-dev preview.

Canonical owner: `python/intelligence/artifacts`. An adapter turns a source Artifact into bytes and
builds the governed tool call that proposes them; the Rust authority verifies the bytes itself and
commits the version with a quality record it wrote. Nothing in this package commits state, holds a
credential, or reports its own work as checked.
"""

from __future__ import annotations
