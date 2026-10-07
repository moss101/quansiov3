"""A safe CommonMark subset: Markdown to the HTML body of a printable document (ADP-006, DOMAIN.md §10.4 C).

What it reads: ATX headings, paragraphs, block quotes, bullet and numbered lists (one level of nesting),
fenced code blocks, thematic breaks, GFM pipe tables, and the inline forms emphasis, strong, code spans,
links, images and autolinks.

What it never does, by construction rather than by filtering afterwards:

* **raw HTML is text.** Every ``<``, ``>``, ``&`` and quote in the source is escaped; a ``<script>`` in a
  document is printed as the characters it is made of;
* **no link is a link.** A link is printed as its text followed by its target in parentheses, so a
  printed page shows where it points and nothing in it can be followed or fetched; a ``javascript:``
  target is printed as text like any other;
* **no image is fetched.** An image is kept only when its target is an inline ``data:`` image
  (png, jpeg, gif or webp); any other target is printed as the image's alt text and the target.

The authority checks the result against its own markup allowlist and prints it in the render sandbox, so
this module is not the only line: it is the one that makes the common case correct.
"""

from __future__ import annotations

import re
from html import escape

_INLINE_IMAGE = re.compile(r"data:image/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/=]+")
_HEADING = re.compile(r"^(#{1,6})[ \t]+(.*?)(?:[ \t]+#+)?[ \t]*$")
_FENCE = re.compile(r"^(```|~~~)[ \t]*([A-Za-z0-9_+.-]*)[^\n]*$")
_RULE = re.compile(r"^[ ]{0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$")
_BULLET = re.compile(r"^([ ]*)([-*+])[ \t]+(.*)$")
_NUMBERED = re.compile(r"^([ ]*)(\d{1,9})[.)][ \t]+(.*)$")
_TABLE_SEPARATOR = re.compile(r"^[ \t]*\|?[ \t]*:?-{1,}:?[ \t]*(\|[ \t]*:?-{1,}:?[ \t]*)*\|?[ \t]*$")


def render(markdown: str) -> str:
    """The HTML body for a Markdown document."""
    lines = markdown.replace("\x00", "").replace("\r\n", "\n").replace("\r", "\n").split("\n")
    return "".join(_blocks(lines))


def _blocks(lines: list[str]) -> list[str]:
    html: list[str] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if not stripped:
            i += 1
            continue
        fence = _FENCE.match(line.strip()) if line.startswith(("```", "~~~")) else None
        if fence:
            marker = fence.group(1)
            code: list[str] = []
            i += 1
            while i < len(lines) and not lines[i].startswith(marker):
                code.append(lines[i])
                i += 1
            i += 1  # the closing fence, or the end
            html.append(f"<pre><code>{escape(chr(10).join(code))}</code></pre>")
            continue
        heading = _HEADING.match(line)
        if heading:
            level = len(heading.group(1))
            html.append(f"<h{level}>{_inline(heading.group(2))}</h{level}>")
            i += 1
            continue
        if _RULE.match(line):
            html.append("<hr>")
            i += 1
            continue
        if line.lstrip().startswith(">"):
            quoted: list[str] = []
            while i < len(lines) and lines[i].lstrip().startswith(">"):
                text = lines[i].lstrip()[1:]
                quoted.append(text[1:] if text.startswith(" ") else text)
                i += 1
            html.append(f"<blockquote>{''.join(_blocks(quoted))}</blockquote>")
            continue
        if _BULLET.match(line) or _NUMBERED.match(line):
            block, i = _list(lines, i)
            html.append(block)
            continue
        if (
            "|" in line
            and i + 1 < len(lines)
            and _TABLE_SEPARATOR.match(lines[i + 1])
            and "-" in lines[i + 1]
        ):
            block, i = _table(lines, i)
            html.append(block)
            continue
        paragraph: list[str] = []
        while i < len(lines) and lines[i].strip() and not _starts_block(lines, i):
            paragraph.append(lines[i])
            i += 1
        html.append(f"<p>{_inline_lines(paragraph)}</p>")
    return html


def _starts_block(lines: list[str], i: int) -> bool:
    line = lines[i]
    return bool(
        _HEADING.match(line)
        or _RULE.match(line)
        or line.startswith(("```", "~~~"))
        or line.lstrip().startswith(">")
        or _BULLET.match(line)
        or _NUMBERED.match(line)
        or (
            "|" in line
            and i + 1 < len(lines)
            and _TABLE_SEPARATOR.match(lines[i + 1])
            and "-" in lines[i + 1]
        )
    )


def _list(lines: list[str], i: int) -> tuple[str, int]:
    first_bullet = _BULLET.match(lines[i])
    ordered = first_bullet is None
    base = len((first_bullet or _NUMBERED.match(lines[i])).group(1))  # type: ignore[union-attr]
    items: list[list[str]] = []
    while i < len(lines):
        line = lines[i]
        match = _BULLET.match(line) or _NUMBERED.match(line)
        if match and len(match.group(1)) <= base + 1 and (_BULLET.match(line) is None) == ordered:
            items.append([match.group(3)])
            i += 1
            continue
        if match and len(match.group(1)) > base + 1 and items:
            # A nested list: the indented run becomes the item's own list.
            nested: list[str] = []
            while (
                i < len(lines)
                and (_BULLET.match(lines[i]) or _NUMBERED.match(lines[i]))
                and len(
                    (_BULLET.match(lines[i]) or _NUMBERED.match(lines[i])).group(1)  # type: ignore[union-attr]
                )
                > base + 1
            ):
                nested.append(lines[i][base + 2 :] if len(lines[i]) > base + 2 else lines[i])
                i += 1
            items[-1].append("\0" + "\n".join(nested))
            continue
        if line.startswith("  ") and line.strip() and items and not match:
            items[-1].append(line.strip())  # a continuation line
            i += 1
            continue
        break
    tag = "ol" if ordered else "ul"
    rendered: list[str] = []
    for item in items:
        text_lines = [part for part in item if not part.startswith("\0")]
        nested_html = "".join(
            "".join(_blocks(part[1:].split("\n"))) for part in item if part.startswith("\0")
        )
        rendered.append(f"<li>{_inline_lines(text_lines)}{nested_html}</li>")
    return f"<{tag}>{''.join(rendered)}</{tag}>", i


def _cells(line: str) -> list[str]:
    row = line.strip()
    if row.startswith("|"):
        row = row[1:]
    if row.endswith("|") and not row.endswith("\\|"):
        row = row[:-1]
    return [cell.strip().replace("\\|", "|") for cell in re.split(r"(?<!\\)\|", row)]


def _table(lines: list[str], i: int) -> tuple[str, int]:
    header = _cells(lines[i])
    i += 2
    body: list[list[str]] = []
    while i < len(lines) and lines[i].strip() and "|" in lines[i]:
        body.append(_cells(lines[i]))
        i += 1
    width = len(header)
    head = "".join(f"<th>{_inline(cell)}</th>" for cell in header)
    rows = "".join(
        "<tr>" + "".join(f"<td>{_inline(cell)}</td>" for cell in (row + [""] * width)[:width]) + "</tr>"
        for row in body
    )
    return f"<table><thead><tr>{head}</tr></thead><tbody>{rows}</tbody></table>", i


def _inline_lines(lines: list[str]) -> str:
    out: list[str] = []
    for index, line in enumerate(lines):
        hard = line.endswith("  ") or line.endswith("\\")
        text = line.strip()
        text = text[:-1] if text.endswith("\\") else text
        out.append(_inline(text))
        if index < len(lines) - 1:
            out.append("<br>" if hard else " ")
    return "".join(out)


# ---------------------------------------------------------------------------------------------------
# Inline
# ---------------------------------------------------------------------------------------------------


def _inline(text: str) -> str:
    """Escape everything, then read code spans, images, links, autolinks, strong and emphasis."""
    spans: list[str] = []

    def stash(html: str) -> str:
        spans.append(html)
        return f"\x00{len(spans) - 1}\x00"

    # Code spans first: what is inside is literal.
    def code(match: re.Match[str]) -> str:
        return stash(f"<code>{escape(match.group(2).strip())}</code>")

    text = re.sub(r"(`+)(.+?)\1", code, text)
    # A backslash before ASCII punctuation makes that character literal (not inside a code span).
    text = re.sub(r"\\([!-/:-@\[-`{-~])", lambda m: stash(escape(m.group(1))), text)

    def image(match: re.Match[str]) -> str:
        alt, target = match.group(1), match.group(2).strip().split(" ")[0]
        if _INLINE_IMAGE.fullmatch(target):
            return stash(f'<img src="{target}" alt="{escape(alt, quote=True)}">')
        shown = f"{alt} ({target})" if alt else target
        return stash(escape(shown))

    text = re.sub(r"!\[([^\]]*)\]\(([^)\s]+(?:\s+\"[^\"]*\")?)\)", image, text)

    def link(match: re.Match[str]) -> str:
        label, target = match.group(1), match.group(2).strip().split(" ")[0]
        return stash(f"{_emphasis(escape(label))} ({escape(target)})")

    text = re.sub(r"\[([^\]]+)\]\(([^)\s]+(?:\s+\"[^\"]*\")?)\)", link, text)
    text = re.sub(r"<((?:https?|mailto):[^<>\s]+)>", lambda m: stash(escape(m.group(1))), text)
    text = _emphasis(escape(text))
    return re.sub(r"\x00(\d+)\x00", lambda m: spans[int(m.group(1))], text)


def _emphasis(escaped: str) -> str:
    escaped = re.sub(r"\*\*(?=\S)(.+?)(?<=\S)\*\*", r"<strong>\1</strong>", escaped)
    escaped = re.sub(r"(?<![\w_])__(?=\S)(.+?)(?<=\S)__(?![\w_])", r"<strong>\1</strong>", escaped)
    escaped = re.sub(r"\*(?=\S)(.+?)(?<=\S)\*", r"<em>\1</em>", escaped)
    escaped = re.sub(r"(?<![\w_])_(?=\S)(.+?)(?<=\S)_(?![\w_])", r"<em>\1</em>", escaped)
    return escaped
