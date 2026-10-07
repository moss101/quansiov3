"""Slide templates (v1): the structured deck rendered to 1280x720 page fragments (ADP-006, DOMAIN.md §10.4 B).

A presentation source (`python/intelligence/artifacts/formats.py`) becomes one fragment per slide, a
`<section class="slide" data-slide="N" data-layout="…">` the authority lays out in the render sandbox,
plus one shared stylesheet. The rendering is **deterministic**: the same slide gives the same bytes, so
a page digest names a slide's content and a re-render of an unchanged slide changes nothing.

Slide text is plain text. It is escaped on the way into markup and nothing in it is read as markup, a
URL or an instruction. Images are the inline ``data:`` images the source carries; charts are SVG built
here from the numbers; nothing is fetched.

The page the browser makes of a fragment is judged by the authority (overflow, contrast, alternative
text, the text it actually says); this module only chooses a density class from the amount of text so a
normal slide fits, and a slide that still does not fit comes back ``failed_quality`` rather than being
quietly shrunk to nothing.
"""

from __future__ import annotations

import hashlib
import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from html import escape
from typing import Any

from intelligence.artifacts.formats import PresentationSource, Slide, parse_presentation
from intelligence.artifacts.models import ArtifactError

TEMPLATE_VERSION = "v1"
PAGE_WIDTH = 1280
PAGE_HEIGHT = 720

INK = "#1a1a1a"
HEADING = "#14355c"
ACCENT = "#1f4e79"
PAPER = "#ffffff"
BAND = "#1f2a44"
BAND_INK = "#ffffff"

CSS = f"""
.slide{{font-family:"Helvetica Neue",Helvetica,Arial,sans-serif;background:{PAPER};color:{INK};padding:56px 72px;display:flex;flex-direction:column}}
.slide h1{{margin:0 0 28px;font-size:52px;line-height:1.1;font-weight:700;color:{HEADING}}}
.slide .body{{font-size:32px;line-height:1.35}}
.slide .body p{{margin:0 0 14px}}
.slide .body ul{{margin:0 0 14px;padding-left:40px}}
.slide .body li{{margin:0 0 10px}}
.slide.d2 .body{{font-size:26px}}
.slide.d2 h1{{font-size:46px;margin-bottom:22px}}
.slide.d3 .body{{font-size:21px;line-height:1.3}}
.slide.d3 h1{{font-size:40px;margin-bottom:18px}}
.slide.layout-title,.slide.layout-section{{background:{BAND};color:{BAND_INK};justify-content:center}}
.slide.layout-title h1,.slide.layout-section h1{{color:{BAND_INK};font-size:68px}}
.slide.layout-title{{align-items:center;text-align:center}}
.slide.layout-title .body{{font-size:34px}}
.slide.layout-section h1{{font-size:76px}}
.slide .cols{{display:flex;gap:56px}}
.slide .cols .col{{flex:1 1 0;min-width:0}}
.slide .media{{display:flex;gap:48px;align-items:flex-start}}
.slide .media figure{{margin:0;flex:0 0 auto}}
.slide .media img{{display:block;max-width:560px;max-height:400px}}
.slide .media .body{{flex:1 1 0;min-width:0}}
.slide.layout-quote{{justify-content:center}}
.slide.layout-quote h1{{font-size:28px;font-weight:400;color:{ACCENT};margin-bottom:36px}}
.slide.layout-quote blockquote{{margin:0;border-left:8px solid {ACCENT};padding-left:36px;font-size:44px;line-height:1.25;font-style:italic}}
.slide.layout-quote blockquote p{{margin:0 0 12px}}
.slide.layout-quote .cite{{margin:28px 0 0 44px;font-size:28px;color:{HEADING}}}
.slide .chart{{margin:0 0 18px}}
.slide .chart svg{{display:block;width:760px;height:360px}}
.slide .chart text{{fill:{INK}}}
""".strip()


@dataclass(frozen=True, slots=True)
class Page:
    """One slide's fragment."""

    index: int
    fragment: str


@dataclass(frozen=True, slots=True)
class RenderedDeck:
    """What `deck.render` is given: the shared stylesheet and one fragment per slide."""

    template_version: str
    css: str
    pages: tuple[Page, ...]
    source_digests: tuple[str, ...]


# ---------------------------------------------------------------------------------------------------
# What a slide says
# ---------------------------------------------------------------------------------------------------


def body_lines(body: str) -> list[str]:
    """The lines of a body a page or a package must say: each non-blank line with a bullet's ``- `` and a
    quote's leading dash removed, the ``---`` column break dropped (the authority's rule, ``deck::body_lines``)."""
    out: list[str] = []
    for raw in body.splitlines():
        line = raw.strip()
        if not line or line == "---":
            continue
        for marker in ("- ", "— "):
            if line.startswith(marker):
                line = line[len(marker) :]
                break
        line = line.strip()
        if line:
            out.append(line)
    return out


def slide_digests(payload: bytes) -> list[str]:
    """The source digest of each slide of a stored presentation: SHA-256 of the slide's mapping as stored,
    with sorted keys, compact separators and unescaped non-ASCII text (the authority's ``SlideSource::digest``)."""
    document = json.loads(payload.decode("utf-8"))
    return [
        hashlib.sha256(
            json.dumps(slide, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        ).hexdigest()
        for slide in document["slides"]
    ]


# ---------------------------------------------------------------------------------------------------
# Blocks
# ---------------------------------------------------------------------------------------------------


def _blocks(text: str) -> str:
    """Bullets become a list, every other non-blank line a paragraph."""
    html: list[str] = []
    bullets: list[str] = []

    def flush() -> None:
        if bullets:
            html.append("<ul>" + "".join(f"<li>{escape(item)}</li>" for item in bullets) + "</ul>")
            bullets.clear()

    for raw in text.splitlines():
        line = raw.strip()
        if not line or line == "---":
            flush()
            continue
        if line.startswith("- "):
            bullets.append(line[2:].strip())
        else:
            flush()
            html.append(f"<p>{escape(line)}</p>")
    flush()
    return "".join(html)


def _density(slide: Slide) -> str:
    lines = body_lines(slide.body)
    weight = sum(len(line) for line in lines) + 40 * len(lines) + len(slide.title) // 2
    if weight <= 420:
        return "d1"
    if weight <= 700:
        return "d2"
    return "d3"


def _fmt(value: float) -> str:
    return str(int(value)) if float(value).is_integer() else f"{value:g}"


def chart_svg(chart: Mapping[str, Any], alt: str) -> str:
    """A bar or line chart as inline SVG, laid out from the numbers alone."""
    labels = [str(label) for label in chart["labels"]]
    values = [float(value) for value in chart["values"]]
    unit = str(chart.get("unit") or "")
    width, height = 760, 360
    left, right, top, bottom = 56, 16, 28, 56
    plot_w, plot_h = width - left - right, height - top - bottom
    low = min(0.0, min(values))
    high = max(0.0, max(values))
    span = (high - low) or 1.0

    def y_of(value: float) -> float:
        return top + plot_h * (1 - (value - low) / span)

    zero = y_of(0.0)
    count = len(values)
    step = plot_w / count
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" '
        f'role="img" aria-label="{escape(alt, quote=True)}"><title>{escape(alt)}</title>',
        f'<line x1="{left}" y1="{zero:.1f}" x2="{width - right}" y2="{zero:.1f}" stroke="{INK}" stroke-width="2"/>',
        f'<line x1="{left}" y1="{top}" x2="{left}" y2="{top + plot_h}" stroke="{INK}" stroke-width="2"/>',
    ]
    points: list[str] = []
    for position, (label, value) in enumerate(zip(labels, values, strict=True)):
        centre = left + step * (position + 0.5)
        y = y_of(value)
        if chart["kind"] == "bar":
            bar_w = step * 0.6
            top_y, bar_h = (y, zero - y) if value >= 0 else (zero, y - zero)
            parts.append(
                f'<rect x="{centre - bar_w / 2:.1f}" y="{top_y:.1f}" width="{bar_w:.1f}" height="{max(bar_h, 1):.1f}" fill="{ACCENT}"/>'
            )
        else:
            points.append(f"{centre:.1f},{y:.1f}")
            parts.append(f'<circle cx="{centre:.1f}" cy="{y:.1f}" r="6" fill="{ACCENT}"/>')
        text = f"{_fmt(value)}{unit}"
        parts.append(
            f'<text x="{centre:.1f}" y="{max(y - 8, 18):.1f}" text-anchor="middle" font-size="16" fill="{INK}">{escape(text)}</text>'
        )
        parts.append(
            f'<text x="{centre:.1f}" y="{height - 22}" text-anchor="middle" font-size="16" fill="{INK}">{escape(label)}</text>'
        )
    if chart["kind"] == "line" and len(points) > 1:
        parts.insert(
            3, f'<polyline points="{" ".join(points)}" fill="none" stroke="{ACCENT}" stroke-width="4"/>'
        )
    parts.append("</svg>")
    return "".join(parts)


# ---------------------------------------------------------------------------------------------------
# Slides
# ---------------------------------------------------------------------------------------------------


def render_slide(slide: Slide, index: int) -> str:
    """One slide's section element."""
    layout = slide.effective_layout
    title = f"<h1>{escape(slide.title)}</h1>"
    body = slide.body
    density = _density(slide)
    inner: str
    if layout == "two_column":
        left, _, right = body.partition("\n---\n")
        inner = (
            f'{title}<div class="body cols"><div class="col">{_blocks(left)}</div>'
            f'<div class="col">{_blocks(right)}</div></div>'
        )
    elif layout == "image":
        assert slide.image is not None and slide.alt is not None
        inner = (
            f'{title}<div class="media"><figure><img src="{slide.image}" alt="{escape(slide.alt, quote=True)}"></figure>'
            f'<div class="body">{_blocks(body)}</div></div>'
        )
    elif layout == "quote":
        lines = [line for line in body.splitlines() if line.strip()]
        cite = ""
        if lines and lines[-1].strip().startswith("— "):
            cite = f'<p class="cite">{escape(lines[-1].strip())}</p>'
            lines = lines[:-1]
        quote = "".join(f"<p>{escape(line.strip())}</p>" for line in lines)
        inner = f"{title}<blockquote>{quote}</blockquote>{cite}"
    elif layout == "chart":
        assert slide.chart is not None and slide.alt is not None
        inner = (
            f'{title}<div class="chart">{chart_svg(slide.chart, slide.alt)}</div>'
            f'<div class="body">{_blocks(body)}</div>'
        )
    else:  # content, title, section
        inner = f'{title}<div class="body">{_blocks(body)}</div>'
    return f'<section class="slide layout-{layout} {density}" data-slide="{index}" data-layout="{layout}">{inner}</section>'


def render_deck(source: PresentationSource | bytes) -> RenderedDeck:
    """Every slide of a presentation, with the stylesheet and the digest of each slide's source."""
    payload = source if isinstance(source, bytes) else source.canonical_bytes()
    parsed = parse_presentation(payload)
    if len(parsed.slides) > 100:
        raise ArtifactError("VALIDATION_BOUNDS", "output.deck.slides", "a deck has at most 100 slides")
    return RenderedDeck(
        template_version=TEMPLATE_VERSION,
        css=CSS,
        pages=tuple(Page(index=i, fragment=render_slide(slide, i)) for i, slide in enumerate(parsed.slides)),
        source_digests=tuple(slide_digests(payload)),
    )


def select_pages(rendered: RenderedDeck, indices: Sequence[int] | None = None) -> tuple[Page, ...]:
    """The pages to hand to `deck.render`: all of them, or only the given slides."""
    if indices is None:
        return rendered.pages
    wanted = set(indices)
    return tuple(page for page in rendered.pages if page.index in wanted)
