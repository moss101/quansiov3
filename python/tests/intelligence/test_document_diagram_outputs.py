"""Printed documents and diagrams (ADP-006, DOMAIN.md §10.4 C): the Markdown subset, the diagram IR, the
Mermaid reader, the deterministic layout and the goldens the authority's tests read.

Set ``QUANSIO_UPDATE_GOLDEN=1`` to rewrite the goldens after an intended change, and review the diff.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest
import yaml
from jsonschema import Draft202012Validator

from intelligence.artifacts import ArtifactError
from intelligence.artifacts.outputs import calls, diagrams, documents, markdown

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "packs" / "skills" / "artifacts" / "fixtures"
GOLDEN = ROOT / "packs" / "skills" / "artifacts" / "golden"
UPDATE = os.environ.get("QUANSIO_UPDATE_GOLDEN") == "1"


def _golden(name: str, content: str) -> None:
    path = GOLDEN / name
    if UPDATE:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    assert path.exists(), f"{name} is missing; run with QUANSIO_UPDATE_GOLDEN=1"
    assert path.read_text(encoding="utf-8") == content, f"{name} differs from what the adapter produces now"


# ---------------------------------------------------------------------------------------------------
# Markdown
# ---------------------------------------------------------------------------------------------------


def test_the_rich_document_renders_to_the_golden_html():
    html = documents.document_html((FIXTURES / "document-rich.md").read_bytes())
    _golden("document-rich.body.html", html + "\n")


def test_each_supported_construct_becomes_its_element():
    html = markdown.render(
        "# One\n\n## Two\n\npara with **strong**, *em*, __strong__, _em_ and `code <b>`.\n\n"
        "- a\n- b\n\n1. x\n2. y\n\n> quoted\n\n---\n\n| h1 | h2 |\n| -- | -- |\n| c1 | c2 |\n"
    )
    for fragment in [
        "<h1>One</h1>",
        "<h2>Two</h2>",
        "<strong>strong</strong>",
        "<em>em</em>",
        "<code>code &lt;b&gt;</code>",
        "<ul><li>a</li><li>b</li></ul>",
        "<ol><li>x</li><li>y</li></ol>",
        "<blockquote><p>quoted</p></blockquote>",
        "<hr>",
        "<table><thead><tr><th>h1</th><th>h2</th></tr></thead><tbody><tr><td>c1</td><td>c2</td></tr></tbody></table>",
    ]:
        assert fragment in html, (fragment, html)


def test_a_nested_list_nests_and_a_code_fence_is_literal():
    html = markdown.render("- one\n  - inner a\n  - inner b\n- two\n\n```py\nx = '<tag>' & y\n```\n")
    assert "<li>one<ul><li>inner a</li><li>inner b</li></ul></li>" in html
    assert "<pre><code>x = &#x27;&lt;tag&gt;&#x27; &amp; y</code></pre>" in html


def test_raw_html_is_text_and_nothing_in_the_source_becomes_markup():
    html = markdown.render(
        "# T\n\n<script>alert(1)</script> <img src=x onerror=alert(1)> &amp; <iframe src=//evil></iframe>\n\n"
        "<div onclick=x>hi</div>\n\n[click](javascript:alert(1)) and [ok](https://example.com/a?b=1&c=2)\n\n"
        "![remote](https://evil.example/track.png) ![x](javascript:alert(1)) <https://auto.example/x>\n\n"
        "```\n</pre><script>x</script>\n```\n"
    )
    # Read the output as a browser would: only the elements the renderer makes, none with an attribute.
    from html.parser import HTMLParser

    seen: list[tuple[str, list[tuple[str, str | None]]]] = []

    class Tags(HTMLParser):
        def handle_starttag(self, tag, attrs):
            seen.append((tag, attrs))

    Tags().feed(html)
    assert {tag for tag, _ in seen} <= {"h1", "p", "pre", "code"}, seen
    assert all(not attrs for _, attrs in seen), seen
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in html
    assert "click (javascript:alert(1))" in html, "a script link is printed as text"
    assert "ok (https://example.com/a?b=1&amp;c=2)" in html
    assert "remote (https://evil.example/track.png)" in html, (
        "a remote image is its alt text and its target, never fetched"
    )
    assert "https://auto.example/x" in html
    assert "&lt;/pre&gt;&lt;script&gt;x&lt;/script&gt;" in html


def test_only_an_inline_data_image_stays_an_image():
    gif = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw=="
    assert f'<img src="{gif}" alt="pixel">' in markdown.render(f"![pixel]({gif})")
    svg = "data:image/svg+xml;base64,PHN2Zz4="
    assert "<img" not in markdown.render(f"![x]({svg})")
    assert "<img" not in markdown.render('![a"onerror="x](data:image/png;base64,AAAA"\'x)')


def test_alt_text_cannot_close_its_attribute():
    gif = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw=="
    html = markdown.render(f'![a" onerror="alert(1)]({gif})')
    assert ' onerror="' not in html
    assert "&quot;" in html


def test_a_backslash_makes_punctuation_literal_except_inside_code():
    assert markdown.render("a \\* b \\_ c \\<d\\>") == "<p>a * b _ c &lt;d&gt;</p>"
    assert markdown.render("`a \\* b`") == "<p><code>a \\* b</code></p>"
    assert "<em>" not in markdown.render("\\*not emphasis\\*")


def test_a_nul_in_the_source_cannot_forge_a_placeholder():
    html = markdown.render("`secret` \x000\x00 text")
    assert "<code>secret</code>" in html
    assert html.count("<code>") == 1


def test_a_document_becomes_the_authoritys_input():
    html = documents.document_html(b"# Title\n\nBody with **bold**.\n")
    assert html.startswith("<h1>Title</h1>") and "<strong>bold</strong>" in html
    with pytest.raises(ArtifactError):
        documents.document_html(b"no heading here")
    with pytest.raises(ArtifactError):
        documents.document_html(b"# T\n\n<script>x</script>")


# ---------------------------------------------------------------------------------------------------
# Diagrams
# ---------------------------------------------------------------------------------------------------


def _mermaid() -> diagrams.Diagram:
    return diagrams.from_mermaid((FIXTURES / "diagram-release.mmd").read_text(), title="Release path")


def test_the_mermaid_subset_reads_into_the_ir():
    ir = _mermaid()
    assert ir.direction == "LR" and ir.kind == "flow"
    assert [(n.id, n.label, n.shape) for n in ir.nodes] == [
        ("A", "Commit", "box"),
        ("B", "Build", "round"),
        ("C", "Tests pass?", "diamond"),
        ("D", "Deploy", "box"),
        ("E", "Done", "round"),
    ]
    assert [(e.source, e.target, e.label) for e in ir.edges] == [
        ("A", "B", None),
        ("B", "C", None),
        ("C", "D", "yes"),
        ("C", "A", "no"),
        ("D", "E", None),
    ]
    chained = diagrams.from_mermaid("graph TD\nX --> Y --> Z;\nZ --> X", title="t")
    assert [(e.source, e.target) for e in chained.edges] == [("X", "Y"), ("Y", "Z"), ("Z", "X")]
    assert chained.direction == "TB"


@pytest.mark.parametrize(
    "source",
    [
        "sequenceDiagram\nA->>B: hi",
        "flowchart TD\nsubgraph s\nA --> B\nend",
        "flowchart TD\nstyle A fill:#f9f",
        "flowchart TD\nclick A callback",
        "flowchart TD\nA --> ",
        "flowchart TD\n--> B",
        "flowchart TD\nA ~~~ B",
        "A --> B",
        "",
        "flowchart TD\n" + " --> ".join(f"N{i}" for i in range(250)),
    ],
)
def test_what_the_subset_does_not_read_is_an_error_not_a_guess(source):
    with pytest.raises(ArtifactError):
        diagrams.from_mermaid(source, title="t")


def test_the_ir_round_trips_and_is_validated_like_the_authority_does():
    ir = _mermaid()
    again = diagrams.parse_ir(ir.canonical_bytes())
    assert again == ir
    bad = json.loads(ir.canonical_bytes())
    for edit in (
        lambda d: d.update(kind="mind"),
        lambda d: d["nodes"].append(dict(d["nodes"][0])),
        lambda d: d["edges"].append({"from": "A", "to": "ZZ"}),
        lambda d: d.update(extra=1),
        lambda d: d["nodes"][0].update(shape="star"),
        lambda d: d["nodes"][0].update(id="1x"),
    ):
        broken = json.loads(json.dumps(bad))
        edit(broken)
        with pytest.raises(ArtifactError):
            diagrams.parse_ir(json.dumps(broken).encode())
    tree = {
        "schema_version": "v1",
        "kind": "tree",
        "title": "t",
        "nodes": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}, {"id": "c", "label": "C"}],
        "edges": [{"from": "a", "to": "c"}, {"from": "b", "to": "c"}],
    }
    with pytest.raises(ArtifactError):
        diagrams.parse_ir(json.dumps(tree).encode())


def test_layout_is_deterministic_and_every_node_and_edge_is_drawn():
    for ir in (
        _mermaid(),
        diagrams.parse_ir((FIXTURES / "diagram-sequence.json").read_bytes()),
        diagrams.parse_ir((FIXTURES / "diagram-tree.json").read_bytes()),
    ):
        svg = diagrams.to_svg(ir)
        assert svg == diagrams.to_svg(ir)
        for node in ir.nodes:
            assert f'data-node="{node.id}"' in svg
        assert svg.count("data-edge=") == len(ir.edges)
        assert svg.startswith('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ')
        assert "<script" not in svg and "href" not in svg


def test_a_label_text_is_escaped_and_a_long_one_wraps_back_into_itself():
    ir = diagrams.Diagram(
        kind="flow",
        title="a <b> & c",
        direction="TB",
        nodes=(
            diagrams.Node(
                "a", 'Tom & "Jerry" <script>alert(1)</script> and a rather long label that must wrap'
            ),
        ),
        edges=(),
    )
    svg = diagrams.to_svg(diagrams.validate(ir))
    assert "<script" not in svg and "&lt;script&gt;" in svg
    assert "<title>a &lt;b&gt; &amp; c</title>" in svg and "<b>" not in svg
    assert (
        diagrams._wrap(ir.nodes[0].label) and "".join(diagrams._wrap(ir.nodes[0].label)) == ir.nodes[0].label
    )


def test_the_goldens_are_what_the_adapter_produces():
    release = _mermaid()
    sequence = diagrams.parse_ir((FIXTURES / "diagram-sequence.json").read_bytes())
    tree = diagrams.parse_ir((FIXTURES / "diagram-tree.json").read_bytes())
    _golden("diagram-release.ir.json", release.canonical_bytes().decode("utf-8") + "\n")
    _golden("diagram-release.svg", diagrams.to_svg(release) + "\n")
    _golden("diagram-sequence.svg", diagrams.to_svg(sequence) + "\n")
    _golden("diagram-tree.svg", diagrams.to_svg(tree) + "\n")


# ---------------------------------------------------------------------------------------------------
# The calls
# ---------------------------------------------------------------------------------------------------


def _schema(tool: str) -> Draft202012Validator:
    registry = yaml.safe_load((ROOT / "config" / "tools.yaml").read_text(encoding="utf-8"))
    return Draft202012Validator(
        next(item for item in registry["tools"] if item["name"] == tool)["input_schema"]
    )


def test_the_calls_are_what_the_tool_registry_accepts():
    html = documents.document_html((FIXTURES / "document-rich.md").read_bytes())
    for call in (
        calls.document_export_pdf_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", html),
        calls.diagram_render_call("art_01J8Z3K6F1N8VQ2X5W9YADP006", diagrams.to_svg(_mermaid())),
    ):
        assert not list(_schema(call.tool).iter_errors(dict(call.arguments))), call.tool
    assert list(_schema("diagram.render").iter_errors({"artifact_id": "a", "svg": "x", "extra": 1}))
