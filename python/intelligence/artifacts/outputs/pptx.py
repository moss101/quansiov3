"""A minimal PresentationML writer for the structured deck (ADP-006, DOMAIN.md §10.4 B).

The package is written with the standard library alone: a ZIP whose parts are **stored** (not
compressed) in a fixed order with fixed timestamps, so the same deck gives the same bytes. It holds
one master, one layout, a theme, a slide per slide of the deck, a notes part for each slide that has
speaker notes, and the media an image slide carries. Text goes in as real text runs (`a:t`), bullets
as paragraphs, a chart as native shapes, an image as a picture with its alt text.

The writer **validates what it wrote before it returns it** (:func:`verify_package`): it reopens the
bytes as a ZIP, parses every XML part, resolves every relationship, and reads each slide's text back
against the source. That is the writer's own check. The authority re-verifies the package part by part
before it stores a version (``outputs::ooxml::verify_pptx``) and does not rely on this one.
"""

from __future__ import annotations

import base64
import io
import zipfile
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any
from xml.etree import ElementTree as ET
from xml.sax.saxutils import escape, quoteattr

from intelligence.artifacts.formats import PresentationSource, Slide, parse_presentation
from intelligence.artifacts.models import ArtifactError
from intelligence.artifacts.outputs.decks import ACCENT, BAND, BAND_INK, HEADING, INK, body_lines

EMU_PER_PX = 9525
SLIDE_W = 12_192_000
SLIDE_H = 6_858_000
NS_A = "http://schemas.openxmlformats.org/drawingml/2006/main"
NS_P = "http://schemas.openxmlformats.org/presentationml/2006/main"
NS_R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_REL = "http://schemas.openxmlformats.org/package/2006/relationships"
NS_CT = "http://schemas.openxmlformats.org/package/2006/content-types"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
XML_DECL = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
ROOT_NS = f'xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"'

CT_PRESENTATION = "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"
CT_SLIDE = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml"
CT_MASTER = "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"
CT_LAYOUT = "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"
CT_NOTES = "application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"
CT_NOTES_MASTER = "application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml"
CT_THEME = "application/vnd.openxmlformats-officedocument.theme+xml"
CT_CORE = "application/vnd.openxmlformats-package.core-properties+xml"
CT_APP = "application/vnd.openxmlformats-officedocument.extended-properties+xml"
CT_RELS = "application/vnd.openxmlformats-package.relationships+xml"

_FIXED_TIME = (1980, 1, 1, 0, 0, 0)
_IMAGE_TYPES = {"png": "image/png", "jpeg": "image/jpeg", "gif": "image/gif", "webp": "image/webp"}


def _px(value: float) -> int:
    return round(value * EMU_PER_PX)


def _hex(color: str) -> str:
    return color.lstrip("#").upper()


# ---------------------------------------------------------------------------------------------------
# Shapes
# ---------------------------------------------------------------------------------------------------


def _run(text: str, *, size: int, bold: bool = False, italic: bool = False, color: str = INK) -> str:
    attrs = f'lang="en-US" sz="{size * 100}"' + (' b="1"' if bold else "") + (' i="1"' if italic else "")
    return (
        f'<a:r><a:rPr {attrs} dirty="0"><a:solidFill><a:srgbClr val="{_hex(color)}"/></a:solidFill>'
        f'<a:latin typeface="Helvetica Neue"/></a:rPr><a:t>{escape(text)}</a:t></a:r>'
    )


def _paragraph(text: str, *, size: int, bullet: bool = False, bold: bool = False, italic: bool = False,
               color: str = INK, align: str = "l") -> str:
    if bullet:
        props = (
            f'<a:pPr marL="342900" indent="-342900" algn="{align}"><a:spcBef><a:spcPts val="600"/></a:spcBef>'
            '<a:buFont typeface="Arial"/><a:buChar char="&#8226;"/></a:pPr>'
        )
    else:
        props = f'<a:pPr marL="0" indent="0" algn="{align}"><a:spcBef><a:spcPts val="600"/></a:spcBef><a:buNone/></a:pPr>'
    return f"<a:p>{props}{_run(text, size=size, bold=bold, italic=italic, color=color)}</a:p>"


def _xfrm(x: float, y: float, w: float, h: float) -> str:
    return f'<a:xfrm><a:off x="{_px(x)}" y="{_px(y)}"/><a:ext cx="{_px(w)}" cy="{_px(h)}"/></a:xfrm>'


def _title(shape_id: int, text: str, *, size: int, color: str, x: float, y: float, w: float, h: float,
           align: str = "l") -> str:
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{shape_id}" name="Title {shape_id}"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>'
        f'<p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr>{_xfrm(x, y, w, h)}</p:spPr>'
        f'<p:txBody><a:bodyPr wrap="square" anchor="ctr"><a:normAutofit/></a:bodyPr><a:lstStyle/>'
        f"{_paragraph(text, size=size, bold=True, color=color, align=align)}</p:txBody></p:sp>"
    )


def _textbox(shape_id: int, name: str, paragraphs: list[str], *, x: float, y: float, w: float, h: float,
             anchor: str = "t") -> str:
    inner = "".join(paragraphs) or '<a:p><a:endParaRPr lang="en-US"/></a:p>'
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{shape_id}" name={quoteattr(name)}/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr>'
        f'<p:spPr>{_xfrm(x, y, w, h)}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr>'
        f'<p:txBody><a:bodyPr wrap="square" lIns="0" tIns="0" rIns="0" bIns="0" anchor="{anchor}"><a:normAutofit/></a:bodyPr>'
        f"<a:lstStyle/>{inner}</p:txBody></p:sp>"
    )


def _rect(shape_id: int, name: str, *, x: float, y: float, w: float, h: float, fill: str) -> str:
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{shape_id}" name={quoteattr(name)}/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>'
        f'<p:spPr>{_xfrm(x, y, w, h)}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>'
        f'<a:solidFill><a:srgbClr val="{_hex(fill)}"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr>'
        '<p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang="en-US"/></a:p></p:txBody></p:sp>'
    )


def _line(shape_id: int, name: str, *, x1: float, y1: float, x2: float, y2: float, color: str, width_px: float) -> str:
    x, y = min(x1, x2), min(y1, y2)
    w, h = abs(x2 - x1), abs(y2 - y1)
    flip = ' flipV="1"' if (y2 < y1) != (x2 < x1) else ""
    return (
        f'<p:cxnSp><p:nvCxnSpPr><p:cNvPr id="{shape_id}" name={quoteattr(name)}/><p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr>'
        f'<p:spPr><a:xfrm{flip}><a:off x="{_px(x)}" y="{_px(y)}"/><a:ext cx="{_px(w)}" cy="{_px(h)}"/></a:xfrm>'
        f'<a:prstGeom prst="line"><a:avLst/></a:prstGeom><a:ln w="{_px(width_px)}">'
        f'<a:solidFill><a:srgbClr val="{_hex(color)}"/></a:solidFill></a:ln></p:spPr></p:cxnSp>'
    )


def _picture(shape_id: int, rel_id: str, alt: str, *, x: float, y: float, w: float, h: float) -> str:
    return (
        f'<p:pic><p:nvPicPr><p:cNvPr id="{shape_id}" name="Picture {shape_id}" descr={quoteattr(alt)}/>'
        '<p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>'
        f'<p:blipFill><a:blip r:embed="{rel_id}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>'
        f'<p:spPr>{_xfrm(x, y, w, h)}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>'
    )


def _group(shape_id: int, name: str, alt: str, children: str, *, x: float, y: float, w: float, h: float) -> str:
    off = f'<a:off x="{_px(x)}" y="{_px(y)}"/><a:ext cx="{_px(w)}" cy="{_px(h)}"/>'
    return (
        f'<p:grpSp><p:nvGrpSpPr><p:cNvPr id="{shape_id}" name={quoteattr(name)} descr={quoteattr(alt)}/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>'
        f'<p:grpSpPr><a:xfrm>{off}<a:chOff x="{_px(x)}" y="{_px(y)}"/><a:chExt cx="{_px(w)}" cy="{_px(h)}"/></a:xfrm></p:grpSpPr>'
        f"{children}</p:grpSp>"
    )


def _fmt(value: float) -> str:
    return str(int(value)) if float(value).is_integer() else f"{value:g}"


def _chart_shapes(first_id: int, chart: Mapping[str, Any], alt: str, *, x: float, y: float, w: float, h: float) -> str:
    labels = [str(item) for item in chart["labels"]]
    values = [float(item) for item in chart["values"]]
    unit = str(chart.get("unit") or "")
    left, top, bottom = 48.0, 28.0, 44.0
    plot_w, plot_h = w - left - 8, h - top - bottom
    low, high = min(0.0, min(values)), max(0.0, max(values))
    span = (high - low) or 1.0

    def y_of(value: float) -> float:
        return y + top + plot_h * (1 - (value - low) / span)

    shape_id = first_id
    out: list[str] = []
    zero = y_of(0.0)
    out.append(_line(shape_id, "Axis", x1=x + left, y1=zero, x2=x + w - 8, y2=zero, color=INK, width_px=2))
    shape_id += 1
    step = plot_w / len(values)
    previous: tuple[float, float] | None = None
    for position, (label, value) in enumerate(zip(labels, values, strict=True)):
        centre = x + left + step * (position + 0.5)
        top_y = y_of(value)
        if chart["kind"] == "bar":
            bar_w = step * 0.6
            out.append(
                _rect(shape_id, f"Bar {position + 1}", x=centre - bar_w / 2, y=min(top_y, zero),
                      w=bar_w, h=max(abs(zero - top_y), 1.0), fill=ACCENT)
            )
        else:
            if previous is not None:
                out.append(_line(shape_id, f"Segment {position}", x1=previous[0], y1=previous[1], x2=centre,
                                 y2=top_y, color=ACCENT, width_px=4))
                shape_id += 1
            out.append(_rect(shape_id, f"Point {position + 1}", x=centre - 5, y=top_y - 5, w=10, h=10, fill=ACCENT))
            previous = (centre, top_y)
        shape_id += 1
        out.append(
            _textbox(shape_id, f"Value {position + 1}",
                     [_paragraph(f"{_fmt(value)}{unit}", size=12, align="ctr")],
                     x=centre - step / 2, y=max(top_y - 24, y), w=step, h=20)
        )
        shape_id += 1
        out.append(
            _textbox(shape_id, f"Label {position + 1}", [_paragraph(label, size=12, align="ctr")],
                     x=centre - step / 2, y=y + h - bottom + 8, w=step, h=24)
        )
        shape_id += 1
    return _group(first_id - 1, "Chart", alt, "".join(out), x=x, y=y, w=w, h=h)


# ---------------------------------------------------------------------------------------------------
# Slides
# ---------------------------------------------------------------------------------------------------

_SPTREE_HEAD = (
    '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>'
    '<p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>'
)


def _body_paragraphs(text: str, *, size: int, color: str = INK, align: str = "l") -> list[str]:
    out: list[str] = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line == "---":
            continue
        if line.startswith("- "):
            out.append(_paragraph(line[2:].strip(), size=size, bullet=True, color=color, align=align))
        else:
            out.append(_paragraph(line, size=size, color=color, align=align))
    return out


def _size_for(slide: Slide) -> int:
    lines = body_lines(slide.body)
    weight = sum(len(line) for line in lines) + 40 * len(lines)
    return 24 if weight <= 420 else 20 if weight <= 700 else 16


@dataclass(frozen=True, slots=True)
class _SlidePart:
    xml: str
    media: tuple[tuple[str, str, bytes], ...]  # (part name, extension, bytes)


def _slide_xml(slide: Slide, index: int) -> _SlidePart:
    layout = slide.effective_layout
    shapes: list[str] = []
    media: list[tuple[str, str, bytes]] = []
    background = ""
    size = _size_for(slide)
    margin_x, margin_y = 56.0, 44.0
    usable_w = 1280 - 2 * margin_x
    if layout in {"title", "section"}:
        background = (
            f'<p:bg><p:bgPr><a:solidFill><a:srgbClr val="{_hex(BAND)}"/></a:solidFill><a:effectLst/></p:bgPr></p:bg>'
        )
        centred = layout == "title"
        shapes.append(
            _title(2, slide.title, size=48 if centred else 54, color=BAND_INK, x=margin_x, y=200, w=usable_w, h=130,
                   align="ctr" if centred else "l")
        )
        shapes.append(
            _textbox(3, "Body", _body_paragraphs(slide.body, size=26, color=BAND_INK, align="ctr" if centred else "l"),
                     x=margin_x, y=350, w=usable_w, h=240)
        )
    elif layout == "two_column":
        shapes.append(_title(2, slide.title, size=36, color=HEADING, x=margin_x, y=margin_y, w=usable_w, h=90))
        left, _, right = slide.body.partition("\n---\n")
        col_w = (usable_w - 40) / 2
        shapes.append(_textbox(3, "Left column", _body_paragraphs(left, size=size), x=margin_x, y=160, w=col_w, h=480))
        shapes.append(_textbox(4, "Right column", _body_paragraphs(right, size=size), x=margin_x + col_w + 40, y=160,
                               w=col_w, h=480))
    elif layout == "image":
        assert slide.image is not None and slide.alt is not None
        header, _, payload = slide.image.partition(";base64,")
        extension = header.removeprefix("data:image/")
        media.append((f"ppt/media/image{index + 1}.{extension}", extension, base64.b64decode(payload)))
        shapes.append(_title(2, slide.title, size=36, color=HEADING, x=margin_x, y=margin_y, w=usable_w, h=90))
        shapes.append(_picture(3, "rId2", slide.alt, x=margin_x, y=160, w=520, h=390))
        shapes.append(_textbox(4, "Body", _body_paragraphs(slide.body, size=size), x=margin_x + 560, y=160, w=usable_w - 560, h=480))
    elif layout == "quote":
        lines = [line for line in slide.body.splitlines() if line.strip()]
        cite = ""
        if lines and lines[-1].strip().startswith("— "):
            cite = lines[-1].strip()
            lines = lines[:-1]
        shapes.append(_title(2, slide.title, size=22, color=ACCENT, x=margin_x, y=margin_y, w=usable_w, h=60))
        paragraphs = [_paragraph(line.strip(), size=34, italic=True) for line in lines]
        shapes.append(_textbox(3, "Quote", paragraphs, x=margin_x + 40, y=200, w=usable_w - 80, h=300, anchor="ctr"))
        if cite:
            shapes.append(_textbox(4, "Attribution", [_paragraph(cite, size=22, color=HEADING)], x=margin_x + 40, y=520, w=usable_w - 80, h=60))
    elif layout == "chart":
        assert slide.chart is not None and slide.alt is not None
        shapes.append(_title(2, slide.title, size=36, color=HEADING, x=margin_x, y=margin_y, w=usable_w, h=90))
        shapes.append(_chart_shapes(10, slide.chart, slide.alt, x=margin_x, y=150, w=700, h=380))
        shapes.append(_textbox(3, "Body", _body_paragraphs(slide.body, size=size), x=margin_x, y=550, w=usable_w, h=130))
    else:  # content
        shapes.append(_title(2, slide.title, size=36, color=HEADING, x=margin_x, y=margin_y, w=usable_w, h=90))
        shapes.append(_textbox(3, "Body", _body_paragraphs(slide.body, size=size), x=margin_x, y=160, w=usable_w, h=480))
    xml = (
        f"{XML_DECL}<p:sld {ROOT_NS}><p:cSld>{background}<p:spTree>{_SPTREE_HEAD}{''.join(shapes)}</p:spTree></p:cSld>"
        "<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"
    )
    return _SlidePart(xml=xml, media=tuple(media))


def _notes_xml(notes: str) -> str:
    paragraphs = "".join(
        f'<a:p><a:r><a:rPr lang="en-US" dirty="0"/><a:t>{escape(line.strip())}</a:t></a:r></a:p>'
        for line in notes.splitlines()
        if line.strip()
    )
    return (
        f"{XML_DECL}<p:notes {ROOT_NS}><p:cSld><p:spTree>{_SPTREE_HEAD}"
        '<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr>'
        '<p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp>'
        '<p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>'
        '<p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/>'
        f"<p:txBody><a:bodyPr/><a:lstStyle/>{paragraphs}</p:txBody></p:sp>"
        "</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>"
    )


# ---------------------------------------------------------------------------------------------------
# Fixed parts
# ---------------------------------------------------------------------------------------------------

_COLOR_MAP = (
    'bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" '
    'accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"'
)


def _theme_xml(name: str) -> str:
    colors = (
        ("dk1", "000000"), ("lt1", "FFFFFF"), ("dk2", "1F2A44"), ("lt2", "EEF1F6"), ("accent1", "1F4E79"),
        ("accent2", "2E75B6"), ("accent3", "5B9BD5"), ("accent4", "A5A5A5"), ("accent5", "ED7D31"),
        ("accent6", "70AD47"), ("hlink", "0563C1"), ("folHlink", "954F72"),
    )
    scheme = "".join(f'<a:{tag}><a:srgbClr val="{value}"/></a:{tag}>' for tag, value in colors)
    fill = '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    line = '<a:ln w="9525" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>'
    return (
        f'{XML_DECL}<a:theme xmlns:a="{NS_A}" name={quoteattr(name)}><a:themeElements>'
        f'<a:clrScheme name="Quansio">{scheme}</a:clrScheme>'
        '<a:fontScheme name="Quansio"><a:majorFont><a:latin typeface="Helvetica Neue"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont>'
        '<a:minorFont><a:latin typeface="Helvetica Neue"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme>'
        f'<a:fmtScheme name="Quansio"><a:fillStyleLst>{fill}{fill}{fill}</a:fillStyleLst>'
        f"<a:lnStyleLst>{line}{line}{line}</a:lnStyleLst>"
        '<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle>'
        "<a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>"
        f"<a:bgFillStyleLst>{fill}{fill}{fill}</a:bgFillStyleLst></a:fmtScheme></a:themeElements>"
        "<a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"
    )


def _master_xml() -> str:
    placeholder = (
        '<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{name}"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>'
        '<p:nvPr><p:ph type="{type}"{idx}/></p:nvPr></p:nvSpPr><p:spPr>{xfrm}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>'
        '<p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US"/><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>'
    )
    title = placeholder.format(id=2, name="Title Placeholder 1", type="title", idx="", xfrm=_xfrm(56, 44, 1168, 90), text="Title")
    body = placeholder.format(id=3, name="Text Placeholder 2", type="body", idx=' idx="1"', xfrm=_xfrm(56, 160, 1168, 480), text="Text")
    style = lambda size: f'<a:lvl1pPr algn="l"><a:defRPr sz="{size}"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill></a:defRPr></a:lvl1pPr>'  # noqa: E731
    return (
        f"{XML_DECL}<p:sldMaster {ROOT_NS}><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg>"
        f"<p:spTree>{_SPTREE_HEAD}{title}{body}</p:spTree></p:cSld><p:clrMap {_COLOR_MAP}/>"
        '<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst>'
        f"<p:txStyles><p:titleStyle>{style(3600)}</p:titleStyle><p:bodyStyle>{style(2400)}</p:bodyStyle>"
        f"<p:otherStyle>{style(1800)}</p:otherStyle></p:txStyles></p:sldMaster>"
    )


def _layout_xml() -> str:
    return (
        f'{XML_DECL}<p:sldLayout {ROOT_NS} type="titleOnly" preserve="1"><p:cSld name="Title Only"><p:spTree>{_SPTREE_HEAD}'
        '<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>'
        '<p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>'
        '<a:p><a:r><a:rPr lang="en-US"/><a:t>Title</a:t></a:r></a:p></p:txBody></p:sp>'
        "</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"
    )


def _notes_master_xml() -> str:
    return (
        f"{XML_DECL}<p:notesMaster {ROOT_NS}><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg>"
        f"<p:spTree>{_SPTREE_HEAD}"
        '<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr>'
        f'<p:nvPr><p:ph type="sldImg" idx="2"/></p:nvPr></p:nvSpPr><p:spPr>{_xfrm(96, 112, 576, 324)}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:sp>'
        '<p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>'
        f'<p:nvPr><p:ph type="body" sz="quarter" idx="3"/></p:nvPr></p:nvSpPr><p:spPr>{_xfrm(96, 460, 576, 380)}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>'
        '<p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US"/><a:t>Notes</a:t></a:r></a:p></p:txBody></p:sp>'
        f"</p:spTree></p:cSld><p:clrMap {_COLOR_MAP}/></p:notesMaster>"
    )


def _rels(items: list[tuple[str, str, str]]) -> str:
    body = "".join(f'<Relationship Id="{rid}" Type="{kind}" Target="{target}"/>' for rid, kind, target in items)
    return f'{XML_DECL}<Relationships xmlns="{NS_REL}">{body}</Relationships>'


def _core_xml(title: str) -> str:
    return (
        f'{XML_DECL}<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" '
        'xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" '
        'xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">'
        f"<dc:title>{escape(title)}</dc:title><dc:creator>Quansio</dc:creator>"
        '<dcterms:created xsi:type="dcterms:W3CDTF">1980-01-01T00:00:00Z</dcterms:created>'
        '<dcterms:modified xsi:type="dcterms:W3CDTF">1980-01-01T00:00:00Z</dcterms:modified></cp:coreProperties>'
    )


def _app_xml(slides: int) -> str:
    return (
        f'{XML_DECL}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">'
        f"<Application>Quansio</Application><Slides>{slides}</Slides></Properties>"
    )


# ---------------------------------------------------------------------------------------------------
# The package
# ---------------------------------------------------------------------------------------------------


def build_pptx(source: PresentationSource | bytes, *, title: str = "Presentation") -> bytes:
    """A PPTX package for a deck, validated before it is returned.

    Raises :class:`ArtifactError` if the writer's own check of what it wrote fails.
    """
    payload = source if isinstance(source, bytes) else source.canonical_bytes()
    deck = parse_presentation(payload)
    parts: list[tuple[str, str]] = []  # (name, content text) in package order
    binaries: list[tuple[str, bytes]] = []
    overrides: list[tuple[str, str]] = []
    extensions: set[str] = set()
    has_notes = any(slide.notes.strip() for slide in deck.slides)

    presentation_rels: list[tuple[str, str, str]] = [("rId1", f"{REL}/slideMaster", "slideMasters/slideMaster1.xml")]
    next_rid = 2
    slide_ids: list[str] = []
    slide_xml: list[tuple[int, _SlidePart, Slide]] = []
    for index, slide in enumerate(deck.slides):
        part = _slide_xml(slide, index)
        slide_xml.append((index, part, slide))
        presentation_rels.append((f"rId{next_rid}", f"{REL}/slide", f"slides/slide{index + 1}.xml"))
        slide_ids.append(f'<p:sldId id="{256 + index}" r:id="rId{next_rid}"/>')
        next_rid += 1
    notes_master_id = ""
    if has_notes:
        presentation_rels.append((f"rId{next_rid}", f"{REL}/notesMaster", "notesMasters/notesMaster1.xml"))
        notes_master_id = f'<p:notesMasterIdLst><p:notesMasterId r:id="rId{next_rid}"/></p:notesMasterIdLst>'
        next_rid += 1
    presentation_rels.append((f"rId{next_rid}", f"{REL}/theme", "theme/theme1.xml"))

    presentation = (
        f"{XML_DECL}<p:presentation {ROOT_NS} saveSubsetFonts=\"1\">"
        '<p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>'
        f'{notes_master_id}<p:sldIdLst>{"".join(slide_ids)}</p:sldIdLst>'
        f'<p:sldSz cx="{SLIDE_W}" cy="{SLIDE_H}"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>'
    )

    parts.append(("_rels/.rels", _rels([
        ("rId1", f"{REL}/officeDocument", "ppt/presentation.xml"),
        ("rId2", "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties", "docProps/core.xml"),
        ("rId3", f"{REL}/extended-properties", "docProps/app.xml"),
    ])))
    parts.append(("docProps/core.xml", _core_xml(title)))
    parts.append(("docProps/app.xml", _app_xml(len(deck.slides))))
    parts.append(("ppt/presentation.xml", presentation))
    parts.append(("ppt/_rels/presentation.xml.rels", _rels(presentation_rels)))
    parts.append(("ppt/slideMasters/slideMaster1.xml", _master_xml()))
    parts.append(("ppt/slideMasters/_rels/slideMaster1.xml.rels", _rels([
        ("rId1", f"{REL}/slideLayout", "../slideLayouts/slideLayout1.xml"),
        ("rId2", f"{REL}/theme", "../theme/theme1.xml"),
    ])))
    parts.append(("ppt/slideLayouts/slideLayout1.xml", _layout_xml()))
    parts.append(("ppt/slideLayouts/_rels/slideLayout1.xml.rels", _rels([
        ("rId1", f"{REL}/slideMaster", "../slideMasters/slideMaster1.xml"),
    ])))
    parts.append(("ppt/theme/theme1.xml", _theme_xml("Quansio")))
    overrides += [
        ("/ppt/presentation.xml", CT_PRESENTATION),
        ("/ppt/slideMasters/slideMaster1.xml", CT_MASTER),
        ("/ppt/slideLayouts/slideLayout1.xml", CT_LAYOUT),
        ("/ppt/theme/theme1.xml", CT_THEME),
        ("/docProps/core.xml", CT_CORE),
        ("/docProps/app.xml", CT_APP),
    ]
    if has_notes:
        parts.append(("ppt/notesMasters/notesMaster1.xml", _notes_master_xml()))
        parts.append(("ppt/notesMasters/_rels/notesMaster1.xml.rels", _rels([
            ("rId1", f"{REL}/theme", "../theme/theme2.xml"),
        ])))
        parts.append(("ppt/theme/theme2.xml", _theme_xml("Quansio notes")))
        overrides += [("/ppt/notesMasters/notesMaster1.xml", CT_NOTES_MASTER), ("/ppt/theme/theme2.xml", CT_THEME)]

    for index, part, slide in slide_xml:
        number = index + 1
        rels = [("rId1", f"{REL}/slideLayout", "../slideLayouts/slideLayout1.xml")]
        for name, extension, data in part.media:
            binaries.append((name, data))
            extensions.add(extension)
            rels.append(("rId2", f"{REL}/image", f"../media/{name.rsplit('/', 1)[1]}"))
        if slide.notes.strip():
            rels.append((f"rId{len(rels) + 1}", f"{REL}/notesSlide", f"../notesSlides/notesSlide{number}.xml"))
            parts.append((f"ppt/notesSlides/notesSlide{number}.xml", _notes_xml(slide.notes)))
            parts.append((f"ppt/notesSlides/_rels/notesSlide{number}.xml.rels", _rels([
                ("rId1", f"{REL}/notesMaster", "../notesMasters/notesMaster1.xml"),
                ("rId2", f"{REL}/slide", f"../slides/slide{number}.xml"),
            ])))
            overrides.append((f"/ppt/notesSlides/notesSlide{number}.xml", CT_NOTES))
        parts.append((f"ppt/slides/slide{number}.xml", part.xml))
        parts.append((f"ppt/slides/_rels/slide{number}.xml.rels", _rels(rels)))
        overrides.append((f"/ppt/slides/slide{number}.xml", CT_SLIDE))

    defaults = [("rels", CT_RELS), ("xml", "application/xml")] + [
        (extension, _IMAGE_TYPES[extension]) for extension in sorted(extensions)
    ]
    content_types = (
        f'{XML_DECL}<Types xmlns="{NS_CT}">'
        + "".join(f'<Default Extension="{ext}" ContentType="{ct}"/>' for ext, ct in defaults)
        + "".join(f'<Override PartName="{name}" ContentType="{ct}"/>' for name, ct in overrides)
        + "</Types>"
    )

    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_STORED) as archive:
        for name, data in [("[Content_Types].xml", content_types), *parts]:
            _write(archive, name, data.encode("utf-8"))
        for name, data in binaries:
            _write(archive, name, data)
    package = buffer.getvalue()
    problems = verify_package(package, deck)
    if problems:
        raise ArtifactError("VALIDATION_SCHEMA", "output.deck.pptx.package", "; ".join(problems[:5]))
    return package


def _write(archive: zipfile.ZipFile, name: str, data: bytes) -> None:
    info = zipfile.ZipInfo(name, date_time=_FIXED_TIME)
    info.compress_type = zipfile.ZIP_STORED
    info.create_system = 0
    info.external_attr = 0o644 << 16
    archive.writestr(info, data)


# ---------------------------------------------------------------------------------------------------
# The writer's own check
# ---------------------------------------------------------------------------------------------------


def _resolve(rels_part: str, target: str) -> str | None:
    if target.startswith("/"):
        segments = target[1:].split("/")
    else:
        base = rels_part.split("/")[:-1]
        if base and base[-1] == "_rels":
            base.pop()
        segments = [*base, *target.split("/")]
    out: list[str] = []
    for segment in segments:
        if segment in ("", "."):
            continue
        if segment == "..":
            if not out:
                return None
            out.pop()
        else:
            out.append(segment)
    return "/".join(out)


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def verify_package(package: bytes, source: PresentationSource) -> list[str]:
    """Problems found reopening a package (empty when it is sound): the ZIP, every XML part, the content
    types, every relationship, the slide list and each slide's text and notes against the source."""
    problems: list[str] = []
    try:
        archive = zipfile.ZipFile(io.BytesIO(package))
    except zipfile.BadZipFile as error:
        return [f"not a ZIP: {error}"]
    names = archive.namelist()
    if len(set(names)) != len(names):
        problems.append("a part name appears twice")
    bad_crc = archive.testzip()
    if bad_crc:
        return [*problems, f"{bad_crc}: CRC mismatch"]
    for info in archive.infolist():
        if info.compress_type != zipfile.ZIP_STORED:
            problems.append(f"{info.filename} is compressed")
        if info.filename.startswith("/") or ".." in info.filename.split("/"):
            problems.append(f"{info.filename} is not a name inside the package")
    trees: dict[str, ET.Element] = {}
    for name in names:
        if name.endswith((".xml", ".rels")):
            try:
                trees[name] = ET.fromstring(archive.read(name))
            except ET.ParseError as error:
                problems.append(f"{name}: {error}")
    content_types = trees.get("[Content_Types].xml")
    if content_types is None:
        return [*problems, "[Content_Types].xml is missing"]
    defaults = {el.get("Extension", "").lower() for el in content_types if _local(el.tag) == "Default"}
    overrides = {el.get("PartName", "").lstrip("/") for el in content_types if _local(el.tag) == "Override"}
    for name in names:
        if name != "[Content_Types].xml" and name not in overrides and name.rsplit(".", 1)[-1].lower() not in defaults:
            problems.append(f"{name} has no content type")
    for name in overrides:
        if name not in names:
            problems.append(f"the content types name {name}, which is not in the package")

    rel_targets: dict[str, list[tuple[str, str, str]]] = {}
    for name, root in trees.items():
        if not name.endswith(".rels"):
            continue
        entries: list[tuple[str, str, str]] = []
        for el in root:
            if el.get("TargetMode", "").lower() == "external":
                problems.append(f"{name}: {el.get('Id')} leaves the package")
                continue
            target = _resolve(name, el.get("Target", ""))
            if target is None or target not in names:
                problems.append(f"{name}: {el.get('Id')} points at {target!r}, which is not in the package")
                continue
            entries.append((el.get("Id", ""), el.get("Type", ""), target))
        rel_targets[name] = entries

    presentation = trees.get("ppt/presentation.xml")
    if presentation is None:
        return [*problems, "ppt/presentation.xml is missing"]
    slide_list = next((el for el in presentation if _local(el.tag) == "sldIdLst"), None)
    rels = {rid: target for rid, _, target in rel_targets.get("ppt/_rels/presentation.xml.rels", [])}
    slide_parts: list[str] = []
    for entry in slide_list if slide_list is not None else []:
        rid = next((value for key, value in entry.attrib.items() if key.endswith("}id")), None)
        if rid not in rels:
            problems.append(f"a slide entry names {rid}, which has no relationship")
        else:
            slide_parts.append(rels[rid])
    if len(slide_parts) != len(source.slides):
        problems.append(f"the presentation lists {len(slide_parts)} slides, the deck has {len(source.slides)}")
        return problems
    for number, (part, slide) in enumerate(zip(slide_parts, source.slides, strict=True), start=1):
        if part != f"ppt/slides/slide{number}.xml":
            problems.append(f"slide {number} is {part}")
        root = trees.get(part)
        if root is None or _local(root.tag) != "sld":
            problems.append(f"{part} is not a p:sld")
            continue
        text = " ".join(" ".join((t.text or "") for t in root.iter(f"{{{NS_A}}}t")).split())
        for wanted in [slide.title, *body_lines(slide.body)]:
            if " ".join(wanted.split()) not in text:
                problems.append(f"slide {number} does not say {wanted[:40]!r}")
        ids = [el.get("id") for el in root.iter(f"{{{NS_P}}}cNvPr")]
        if len(ids) != len(set(ids)):
            problems.append(f"slide {number} repeats a shape id")
        if slide.notes.strip():
            notes_part = next(
                (target for _, kind, target in rel_targets.get(f"ppt/slides/_rels/slide{number}.xml.rels", [])
                 if kind.endswith("/notesSlide")),
                None,
            )
            notes_root = trees.get(notes_part or "")
            notes_text = " ".join(" ".join((t.text or "") for t in notes_root.iter(f"{{{NS_A}}}t")).split()) if notes_root is not None else ""
            for line in slide.notes.splitlines():
                if line.strip() and " ".join(line.split()) not in notes_text:
                    problems.append(f"slide {number}: the notes part does not hold {line.strip()[:40]!r}")
    return problems
