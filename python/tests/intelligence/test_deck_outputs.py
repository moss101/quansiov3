"""Slides: templates, PPTX writer and tool calls (ADP-006, DOMAIN.md §10.4 B).

The goldens in `packs/skills/artifacts/golden/` are what the plane produces for the layouts fixture;
the Rust authority's tests (`crates/server/tests/outputs_deck.rs`) read the same files, so what the
plane builds is what the authority is shown. Set ``QUANSIO_UPDATE_GOLDEN=1`` to rewrite them after an
intended change, and review the diff.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
import zipfile
from pathlib import Path

import pytest
import yaml
from jsonschema import Draft202012Validator

from intelligence.artifacts import ArtifactError
from intelligence.artifacts.formats import parse_presentation
from intelligence.artifacts.outputs import calls, decks, pptx

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "packs" / "skills" / "artifacts" / "fixtures"
GOLDEN = ROOT / "packs" / "skills" / "artifacts" / "golden"
LAYOUTS = FIXTURES / "slides-layouts.json"
UPDATE = os.environ.get("QUANSIO_UPDATE_GOLDEN") == "1"


def _deck():
    return parse_presentation(LAYOUTS.read_bytes())


def _render_golden() -> dict:
    deck = _deck()
    rendered = decks.render_deck(deck)
    package = pptx.build_pptx(deck, title="Layouts")
    with zipfile.ZipFile(io.BytesIO(package)) as archive:
        parts = {
            info.filename: hashlib.sha256(archive.read(info.filename)).hexdigest()
            for info in archive.infolist()
        }
    return {
        "template_version": rendered.template_version,
        "css": rendered.css,
        "pages": [{"index": page.index, "fragment": page.fragment} for page in rendered.pages],
        "source_digests": list(rendered.source_digests),
        "pptx_sha256": hashlib.sha256(package).hexdigest(),
        "pptx_parts": parts,
    }


def _check_or_update(path: Path, content: bytes) -> None:
    if UPDATE:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    assert path.exists(), f"{path} is missing; run with QUANSIO_UPDATE_GOLDEN=1"
    assert path.read_bytes() == content, f"{path.name} differs from what the adapter produces now"


def test_the_plane_produces_the_golden_pages_digests_and_package_parts():
    produced = _render_golden()
    _check_or_update(
        GOLDEN / "slides-layouts.render.json",
        (json.dumps(produced, indent=2, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8"),
    )


def test_an_edited_deck_has_one_new_slide_digest_and_one_new_page():
    """The deck after one slide was edited (`slides-layouts-edited.json`): the other digests do not move."""
    before = json.loads((GOLDEN / "slides-layouts.render.json").read_text(encoding="utf-8"))
    edited_bytes = (FIXTURES / "slides-layouts-edited.json").read_bytes()
    rendered = decks.render_deck(edited_bytes)
    changed = [
        i
        for i, (a, b) in enumerate(zip(before["source_digests"], rendered.source_digests, strict=True))
        if a != b
    ]
    assert changed == [1]
    assert [p.fragment for i, p in enumerate(rendered.pages) if i != 1] == [
        p["fragment"] for i, p in enumerate(before["pages"]) if i != 1
    ]
    produced = {
        "template_version": rendered.template_version,
        "css": rendered.css,
        "pages": [{"index": page.index, "fragment": page.fragment} for page in rendered.pages],
        "source_digests": list(rendered.source_digests),
    }
    _check_or_update(
        GOLDEN / "slides-layouts-edited.render.json",
        (json.dumps(produced, indent=2, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8"),
    )


def test_the_golden_package_is_the_bytes_the_writer_produces():
    _check_or_update(GOLDEN / "slides-layouts.pptx", pptx.build_pptx(_deck(), title="Layouts"))


def test_rendering_is_deterministic():
    deck = _deck()
    assert decks.render_deck(deck) == decks.render_deck(deck)
    assert pptx.build_pptx(deck) == pptx.build_pptx(deck)


def test_every_layout_has_a_page_with_the_slide_attributes_the_authority_requires():
    rendered = decks.render_deck(_deck())
    assert [page.index for page in rendered.pages] == list(range(7))
    layouts = ["title", "content", "two_column", "image", "chart", "quote", "section"]
    for page, layout in zip(rendered.pages, layouts, strict=True):
        assert page.fragment.startswith(f'<section class="slide layout-{layout} ')
        assert f'data-slide="{page.index}"' in page.fragment
        assert f'data-layout="{layout}"' in page.fragment
        assert page.fragment.endswith("</section>")


def test_slide_text_is_escaped_and_never_read_as_markup():
    hostile = {
        "schema_version": "v1",
        "slides": [
            {
                "title": '<script>alert(1)</script> & "quotes"',
                "body": "- <img src=x onerror=alert(1)>\n- javascript:alert(1)\n- a < b > c",
                "notes": "<b>notes</b>",
            }
        ],
    }
    fragment = decks.render_deck(json.dumps(hostile).encode()).pages[0].fragment
    assert "<script" not in fragment and "<img" not in fragment
    assert "&lt;script&gt;" in fragment and "&lt;img src=x onerror=alert(1)&gt;" in fragment
    assert "&amp;" in fragment and "&quot;quotes&quot;" in fragment
    # The notes are never on the page.
    assert "notes" not in fragment


def test_alt_text_is_an_attribute_and_cannot_close_it():
    source = {
        "schema_version": "v1",
        "slides": [
            {
                "title": "Pic",
                "body": "x",
                "layout": "image",
                "alt": 'a" onerror="alert(1)',
                "image": "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==",
            }
        ],
    }
    fragment = decks.render_deck(json.dumps(source).encode()).pages[0].fragment
    assert 'alt="a&quot; onerror=&quot;alert(1)"' in fragment
    assert ' onerror="' not in fragment


def test_a_denser_slide_gets_a_denser_class_so_a_normal_slide_fits():
    short = decks.render_slide(
        parse_presentation(
            json.dumps({"schema_version": "v1", "slides": [{"title": "T", "body": "short"}]}).encode()
        ).slides[0],
        0,
    )
    long_body = "\n".join(f"- point number {n} that says something quite long indeed" for n in range(12))
    long = decks.render_slide(
        parse_presentation(
            json.dumps({"schema_version": "v1", "slides": [{"title": "T", "body": long_body}]}).encode()
        ).slides[0],
        0,
    )
    assert ' d1"' in short and ' d3"' in long


def test_slide_digests_follow_the_stored_mapping_and_match_the_authority():
    golden = json.loads((GOLDEN / "slides-layouts.render.json").read_text(encoding="utf-8"))
    assert decks.slide_digests(LAYOUTS.read_bytes()) == golden["source_digests"]
    # The same value the authority's unit test pins for `{"body":"x","title":"Plan"}`.
    one = json.dumps({"schema_version": "v1", "slides": [{"title": "Plan", "body": "x"}]}).encode()
    assert decks.slide_digests(one) == [hashlib.sha256(b'{"body":"x","title":"Plan"}').hexdigest()]
    non_ascii = json.dumps(
        {"schema_version": "v1", "slides": [{"title": "Café", "body": "—"}]}, ensure_ascii=False
    ).encode()
    assert decks.slide_digests(non_ascii) == [
        hashlib.sha256('{"body":"—","title":"Café"}'.encode()).hexdigest()
    ]


def test_body_lines_are_what_a_page_must_say():
    assert decks.body_lines("Intro line\n\n- first\n-  second \n---\n— Ada Lovelace\n   \nplain - dash") == [
        "Intro line",
        "first",
        "second",
        "Ada Lovelace",
        "plain - dash",
    ]
    assert decks.body_lines("---\n\n") == []


# ---------------------------------------------------------------------------------------------------
# The PPTX writer's own check
# ---------------------------------------------------------------------------------------------------


def _package() -> bytes:
    return pptx.build_pptx(_deck(), title="Layouts")


def _rewrite(package: bytes, edit) -> bytes:
    out = io.BytesIO()
    with (
        zipfile.ZipFile(io.BytesIO(package)) as source,
        zipfile.ZipFile(out, "w", zipfile.ZIP_STORED) as target,
    ):
        for info in source.infolist():
            data = edit(info.filename, source.read(info.filename))
            if data is not None:
                target.writestr(info, data)
    return out.getvalue()


def test_a_package_is_a_stored_zip_with_every_part_the_deck_needs():
    package = _package()
    with zipfile.ZipFile(io.BytesIO(package)) as archive:
        names = archive.namelist()
        assert names[0] == "[Content_Types].xml"
        assert all(info.compress_type == zipfile.ZIP_STORED for info in archive.infolist())
        assert [n for n in names if n.startswith("ppt/slides/slide")] == [
            f"ppt/slides/slide{i}.xml" for i in range(1, 8)
        ]
        assert "ppt/notesSlides/notesSlide2.xml" in names, "the second slide has speaker notes"
        assert "ppt/notesSlides/notesSlide1.xml" not in names
        assert "ppt/media/image4.gif" in names, "the image slide embeds its picture"
        assert archive.testzip() is None
    assert pptx.verify_package(package, _deck()) == []


def test_a_slide_picture_carries_its_alt_text_and_a_chart_is_a_named_group():
    with zipfile.ZipFile(io.BytesIO(_package())) as archive:
        image_slide = archive.read("ppt/slides/slide4.xml").decode()
        chart_slide = archive.read("ppt/slides/slide5.xml").decode()
    assert 'descr="A single white pixel, enlarged"' in image_slide
    assert 'descr="Bar chart: North 12, South 9, East 15, West 7 million"' in chart_slide
    assert "<a:t>North</a:t>" in chart_slide and "<a:t>15M</a:t>" in chart_slide


def test_speaker_notes_are_in_the_notes_part_not_on_the_slide():
    with zipfile.ZipFile(io.BytesIO(_package())) as archive:
        slide = archive.read("ppt/slides/slide2.xml").decode()
        notes = archive.read("ppt/notesSlides/notesSlide2.xml").decode()
    assert "Lead with the churn number." in notes
    assert "Lead with the churn number." not in slide


@pytest.mark.parametrize(
    ("label", "edit", "expect"),
    [
        (
            "a slide's text is changed",
            lambda n, d: d.replace(b"Churn is down", b"Churn is up") if n == "ppt/slides/slide2.xml" else d,
            "does not say",
        ),
        (
            "a slide part is dropped",
            lambda n, d: None if n == "ppt/slides/slide3.xml" else d,
            "which is not in the package",
        ),
        ("an xml part is broken", lambda n, d: d + b"<" if n == "ppt/theme/theme1.xml" else d, "theme1.xml"),
        (
            "a content type is lost",
            lambda n, d: (
                d.replace(b'<Default Extension="gif"', b'<Default Extension="zzz"')
                if n == "[Content_Types].xml"
                else d
            ),
            "no content type",
        ),
        (
            "a relationship leaves the package",
            lambda n, d: (
                d.replace(
                    b'Target="../slideLayouts/slideLayout1.xml"',
                    b'Target="https://example.com/x" TargetMode="External"',
                    1,
                )
                if n == "ppt/slides/_rels/slide1.xml.rels"
                else d
            ),
            "leaves the package",
        ),
        (
            "a relationship points at nothing",
            lambda n, d: (
                d.replace(b"slideLayout1.xml", b"missing.xml", 1)
                if n == "ppt/slides/_rels/slide1.xml.rels"
                else d
            ),
            "not in the package",
        ),
        (
            "the notes are emptied",
            lambda n, d: (
                d.replace(b"Lead with the churn number.", b"x")
                if n == "ppt/notesSlides/notesSlide2.xml"
                else d
            ),
            "notes part",
        ),
    ],
)
def test_a_damaged_package_fails_the_writers_own_check(label, edit, expect):
    damaged = _rewrite(_package(), edit)
    problems = pptx.verify_package(damaged, _deck())
    assert problems, label
    assert any(expect in problem for problem in problems), (label, problems)


def test_a_package_for_another_deck_or_a_corrupt_zip_fails():
    other = parse_presentation(
        json.dumps({"schema_version": "v1", "slides": [{"title": "Other", "body": "deck"}]}).encode()
    )
    assert any("lists 7 slides" in p for p in pptx.verify_package(_package(), other))
    assert pptx.verify_package(b"not a zip", _deck())
    package = bytearray(_package())
    at = bytes(package).index(b"Quarterly review")
    package[at] ^= 0x20
    assert any("CRC" in p for p in pptx.verify_package(bytes(package), _deck()))


def test_an_unwritable_deck_is_refused_before_a_package_exists():
    with pytest.raises(ArtifactError):
        pptx.build_pptx(b'{"schema_version": "v1", "slides": []}')


# ---------------------------------------------------------------------------------------------------
# The calls
# ---------------------------------------------------------------------------------------------------


def _schema(tool: str) -> Draft202012Validator:
    registry = yaml.safe_load((ROOT / "config" / "tools.yaml").read_text(encoding="utf-8"))
    declaration = next(item for item in registry["tools"] if item["name"] == tool)
    return Draft202012Validator(declaration["input_schema"])


def test_every_deck_call_is_what_the_tool_registry_accepts():
    deck = _deck()
    rendered = decks.render_deck(deck)
    package = pptx.build_pptx(deck)
    for call in [
        calls.deck_render_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", rendered),
        calls.deck_render_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", rendered, rendered.pages[2:4]),
        calls.deck_export_pdf_call("art_01J8Z3K6F1N8VQ2X5W9YADP006"),
        calls.deck_export_pptx_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", package),
        calls.deck_deliver_call("art_01J8Z3K6F1N8VQ2X5W9YADP006"),
    ]:
        errors = sorted(_schema(call.tool).iter_errors(dict(call.arguments)), key=str)
        assert not errors, (call.tool, [e.message for e in errors][:3])


def test_a_call_with_an_extra_argument_is_not_what_the_registry_accepts():
    call = calls.deck_deliver_call("art_x")
    assert list(_schema(call.tool).iter_errors({**call.arguments, "approved": True}))
