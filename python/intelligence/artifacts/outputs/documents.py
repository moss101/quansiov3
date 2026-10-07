"""Markdown documents to printable HTML (ADP-006, DOMAIN.md §10.4 C).

A ``document`` Artifact (``# Title`` and a Markdown body) becomes the HTML *body* the authority prints:
the title as the first heading, then the body through :mod:`intelligence.artifacts.outputs.markdown`.
The authority composes the page around it (its own print stylesheet and content-security-policy), checks
the markup against its allowlist, prints it in the render sandbox where nothing can be fetched, and
checks the rendered text against the source's words.
"""

from __future__ import annotations

from intelligence.artifacts.formats import DocumentSource, parse_document
from intelligence.artifacts.models import ArtifactError
from intelligence.artifacts.outputs import markdown
from intelligence.artifacts.outputs.limits import shipped_config


def document_html(source: DocumentSource | bytes) -> str:
    """The HTML body of a document, bounded by ``config/outputs.yaml``."""
    parsed = parse_document(source if isinstance(source, bytes) else source.canonical_bytes())
    html = markdown.render(f"# {parsed.title}\n\n{parsed.body}")
    limit = int(shipped_config().pdf_document["html_max_bytes"])
    if len(html.encode("utf-8")) > limit:
        raise ArtifactError(
            "VALIDATION_BOUNDS", "output.document.pdf.size", f"the printable HTML is over {limit} bytes"
        )
    return html
