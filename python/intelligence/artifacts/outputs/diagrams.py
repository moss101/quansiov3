"""Diagrams: a typed IR, a Mermaid flowchart subset, a deterministic layout and the SVG it makes (ADP-006,
DOMAIN.md §10.4 C).

The source of a diagram is a graph (``application/vnd.quansio.diagram+json``): nodes with ids and labels,
edges between them, and a kind (``flow``, ``sequence`` or ``tree``). The plane lays it out with a small
layered algorithm — no external binary, no randomness — and writes SVG in which each node is a
``<g data-node="id">`` holding its label and each edge a ``<path data-edge="from->to">``. The authority
checks the SVG against the source it was made from: one element per node with its label, one per edge,
and nothing a picture should not carry (script, links, frames, external references).

The Mermaid reader is a *subset* and says so: ``flowchart``/``graph`` with ``TD|TB|LR|BT|RL``, node
shapes ``[box]``, ``(round)``, ``((round))`` and ``{diamond}``, edges ``-->``, ``---``, ``-.->`` and
``==>`` with ``|label|`` or ``-- label -->``, chains, ``%%`` comments and ``;`` separators. Anything
else (subgraphs, styling, click handlers, other diagram types) is an error naming the line, never
silently dropped.
"""

from __future__ import annotations

import json
import re
from collections import defaultdict
from dataclasses import dataclass, field
from html import escape

from intelligence.artifacts.models import ArtifactError

MEDIA_TYPE = "application/vnd.quansio.diagram+json"
RULE = "output.diagram.source"
KINDS = ("flow", "sequence", "tree")
SHAPES = ("box", "round", "diamond", "actor")
_ID = re.compile(r"[A-Za-z][A-Za-z0-9_-]{0,31}")

INK = "#1a1a1a"
LINE = "#1f4e79"
FILL = "#eef3f9"


def _error(detail: str) -> ArtifactError:
    return ArtifactError("VALIDATION_SCHEMA", RULE, detail)


@dataclass(frozen=True, slots=True)
class Node:
    id: str
    label: str
    shape: str | None = None


@dataclass(frozen=True, slots=True)
class Edge:
    source: str
    target: str
    label: str | None = None


@dataclass(frozen=True, slots=True)
class Diagram:
    kind: str
    title: str
    nodes: tuple[Node, ...]
    edges: tuple[Edge, ...]
    direction: str | None = None

    def canonical_bytes(self) -> bytes:
        payload: dict[str, object] = {
            "schema_version": "v1",
            "kind": self.kind,
            "title": self.title,
            "nodes": [
                {"id": n.id, "label": n.label, **({"shape": n.shape} if n.shape else {})} for n in self.nodes
            ],
            "edges": [
                {"from": e.source, "to": e.target, **({"label": e.label} if e.label else {})}
                for e in self.edges
            ],
        }
        if self.direction:
            payload["direction"] = self.direction
        return json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def validate(diagram: Diagram, *, max_nodes: int = 200, max_edges: int = 400) -> Diagram:
    """The authority's rules (``svg::DiagramIr::validate``), applied before anything is proposed."""
    if diagram.kind not in KINDS:
        raise _error("kind must be flow, sequence or tree")
    if not diagram.title.strip() or len(diagram.title) > 120:
        raise _error("the title must be 1 to 120 characters")
    if diagram.direction not in (None, "TB", "LR"):
        raise _error("direction must be TB or LR")
    if not diagram.nodes or len(diagram.nodes) > max_nodes:
        raise _error(f"a diagram has 1 to {max_nodes} nodes")
    if len(diagram.edges) > max_edges:
        raise _error(f"a diagram has at most {max_edges} edges")
    ids: set[str] = set()
    for node in diagram.nodes:
        if not _ID.fullmatch(node.id):
            raise _error(f"{node.id!r} is not a node id")
        if node.id in ids:
            raise _error(f"node {node.id} appears twice")
        ids.add(node.id)
        if not node.label.strip() or len(node.label) > 120:
            raise _error(f"node {node.id} needs a label of 1 to 120 characters")
        if node.shape not in (None, *SHAPES):
            raise _error(f"node {node.id} has an unknown shape")
    for edge in diagram.edges:
        if edge.source not in ids or edge.target not in ids:
            raise _error(f"the edge {edge.source}->{edge.target} names a node that does not exist")
        if edge.label is not None and len(edge.label) > 120:
            raise _error("an edge label is at most 120 characters")
    if diagram.kind == "tree":
        parent: dict[str, str] = {}
        for edge in diagram.edges:
            if edge.target in parent:
                raise _error(f"in a tree, {edge.target} has more than one parent")
            parent[edge.target] = edge.source
        for node in diagram.nodes:
            at = node.id
            for _ in range(len(diagram.nodes) + 1):
                if at not in parent:
                    break
                at = parent[at]
                if at == node.id:
                    raise _error(f"in a tree, {node.id} is its own ancestor")
    return diagram


def parse_ir(payload: bytes) -> Diagram:
    """Parse a stored diagram source."""
    try:
        raw = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise _error("the diagram source is not JSON") from error
    if not isinstance(raw, dict):
        raise _error("the diagram source must be a JSON object")
    extra = sorted(set(raw) - {"schema_version", "kind", "title", "direction", "nodes", "edges"})
    if extra:
        raise _error(f"the diagram has unknown keys: {', '.join(extra)}")
    if raw.get("schema_version") != "v1":
        raise _error("schema_version must be v1")
    nodes = []
    for entry in raw.get("nodes", []):
        if not isinstance(entry, dict) or set(entry) - {"id", "label", "shape"}:
            raise _error("a node is {id, label, shape?}")
        nodes.append(Node(str(entry.get("id")), str(entry.get("label")), entry.get("shape")))
    edges = []
    for entry in raw.get("edges", []):
        if not isinstance(entry, dict) or set(entry) - {"from", "to", "label"}:
            raise _error("an edge is {from, to, label?}")
        edges.append(Edge(str(entry.get("from")), str(entry.get("to")), entry.get("label")))
    return validate(
        Diagram(
            kind=str(raw.get("kind")),
            title=str(raw.get("title")),
            nodes=tuple(nodes),
            edges=tuple(edges),
            direction=raw.get("direction"),
        )
    )


# ---------------------------------------------------------------------------------------------------
# Mermaid subset
# ---------------------------------------------------------------------------------------------------

_NODE = re.compile(
    r"""^\s*(?P<id>[A-Za-z][A-Za-z0-9_-]*)
        (?:\s*(?:
            \(\((?P<circle>[^()]*)\)\)
          | \((?P<round>[^()]*)\)
          | \[(?P<box>[^\[\]]*)\]
          | \{(?P<diamond>[^{}]*)\}
        ))?""",
    re.VERBOSE,
)
_ARROW = re.compile(
    r"""^\s*(?:
        (?P<plain>-->|---|==>|-\.->)(?:\s*\|(?P<piped>[^|]*)\|)?
      | (?P<open>--|==|-\.)\s+(?P<text>[^-=>.][^>]*?)\s+(?P<close>-->|==>|\.->|---)
    )""",
    re.VERBOSE,
)


def from_mermaid(source: str, *, title: str) -> Diagram:
    """A flowchart in the supported subset, as the IR."""
    nodes: dict[str, Node] = {}
    edges: list[Edge] = []
    direction: str | None = None
    header_seen = False
    for number, raw in enumerate(source.splitlines(), start=1):
        line = raw.split("%%", 1)[0].strip()
        if not line:
            continue
        if not header_seen:
            match = re.fullmatch(r"(?:flowchart|graph)(?:\s+(TD|TB|LR|BT|RL))?\s*;?", line)
            if not match:
                raise _error(f"line {number}: only `flowchart` or `graph` diagrams are supported")
            header_seen = True
            direction = {"LR": "LR", "RL": "LR"}.get(match.group(1) or "TD", "TB")
            continue
        for statement in line.split(";"):
            statement = statement.strip()
            if not statement:
                continue
            if re.match(r"(subgraph|end|style|classDef|class|click|linkStyle)\b", statement):
                raise _error(f"line {number}: {statement.split()[0]} is not supported in the Mermaid subset")
            _statement(statement, number, nodes, edges)
    if not header_seen:
        raise _error("no `flowchart` or `graph` header")
    return validate(
        Diagram(
            kind="flow", title=title, nodes=tuple(nodes.values()), edges=tuple(edges), direction=direction
        )
    )


def _statement(statement: str, number: int, nodes: dict[str, Node], edges: list[Edge]) -> None:
    rest = statement
    previous: str | None = None
    pending_label: str | None = None
    while rest.strip():
        match = _NODE.match(rest)
        if not match:
            raise _error(f"line {number}: cannot read a node at {rest.strip()[:30]!r}")
        node_id = match.group("id")
        if len(node_id) > 32:
            raise _error(f"line {number}: node id {node_id!r} is longer than 32 characters")
        label = next(
            (match.group(g) for g in ("circle", "round", "box", "diamond") if match.group(g) is not None),
            None,
        )
        shape = (
            "round"
            if match.group("circle") is not None or match.group("round") is not None
            else "diamond"
            if match.group("diamond") is not None
            else "box"
            if match.group("box") is not None
            else None
        )
        if label is not None:
            label = label.strip().strip('"')
            nodes[node_id] = Node(node_id, label, shape)
        elif node_id not in nodes:
            nodes[node_id] = Node(node_id, node_id)
        if previous is not None:
            edges.append(Edge(previous, node_id, pending_label))
        previous = node_id
        rest = rest[match.end() :]
        if not rest.strip():
            return
        arrow = _ARROW.match(rest)
        if not arrow:
            raise _error(f"line {number}: expected an edge at {rest.strip()[:30]!r}")
        pending_label = (arrow.group("piped") or arrow.group("text") or "").strip().strip('"') or None
        rest = rest[arrow.end() :]
        if not rest.strip():
            raise _error(f"line {number}: an edge needs a node at its end")


# ---------------------------------------------------------------------------------------------------
# Layout and SVG
# ---------------------------------------------------------------------------------------------------


@dataclass(slots=True)
class _Box:
    node: Node
    w: float = 0
    h: float = 0
    x: float = 0  # centre
    y: float = 0
    lines: list[str] = field(default_factory=list)
    layer: int = 0


def _wrap(label: str, width: int = 22) -> list[str]:
    """Break a label at spaces; each piece but the last keeps its trailing space so the pieces join back
    into the label exactly."""
    words = label.split(" ")
    lines: list[str] = []
    current = ""
    for index, word in enumerate(words):
        piece = word + (" " if index < len(words) - 1 else "")
        if current and len((current + piece).rstrip()) > width:
            lines.append(current)
            current = piece
        else:
            current += piece
    lines.append(current)
    return lines


def _size(box: _Box) -> None:
    box.lines = _wrap(box.node.label)
    longest = max(len(line.rstrip()) for line in box.lines)
    box.w = max(96.0, longest * 9.0 + 32.0)
    box.h = max(48.0, 24.0 * len(box.lines) + 24.0)
    if box.node.shape == "diamond":
        box.w, box.h = box.w * 1.35, box.h * 1.5


def _layers(boxes: dict[str, _Box], edges: list[Edge]) -> list[list[str]]:
    """Longest-path layering with back edges (found by depth-first search) left out."""
    out: dict[str, list[str]] = defaultdict(list)
    for edge in edges:
        out[edge.source].append(edge.target)
    state: dict[str, int] = {}
    back: set[tuple[str, str]] = set()

    def visit(at: str) -> None:
        state[at] = 1
        for target in out[at]:
            if state.get(target) == 1:
                back.add((at, target))
            elif target not in state:
                visit(target)
        state[at] = 2

    for node_id in boxes:
        if node_id not in state:
            visit(node_id)
    layer = dict.fromkeys(boxes, 0)
    for _ in range(len(boxes)):
        changed = False
        for edge in edges:
            if (edge.source, edge.target) in back or edge.source == edge.target:
                continue
            if layer[edge.target] < layer[edge.source] + 1:
                layer[edge.target] = layer[edge.source] + 1
                changed = True
        if not changed:
            break
    count = max(layer.values()) + 1
    rows: list[list[str]] = [[] for _ in range(count)]
    for node_id in boxes:
        rows[layer[node_id]].append(node_id)
    # Barycentre sweeps reduce crossings; ties keep source order, so the result is deterministic.
    position = {node_id: index for row in rows for index, node_id in enumerate(row)}
    for sweep in range(4):
        order = range(1, count) if sweep % 2 == 0 else range(count - 2, -1, -1)
        for r in order:
            neighbour = r - 1 if sweep % 2 == 0 else r + 1

            def centre(node_id: str, neighbour: int = neighbour) -> float:
                linked = [
                    position[e.source if e.target == node_id else e.target]
                    for e in edges
                    if node_id in (e.source, e.target)
                    and layer[e.source if e.target == node_id else e.target] == neighbour
                ]
                return sum(linked) / len(linked) if linked else position[node_id]

            rows[r].sort(key=lambda node_id: (centre(node_id), position[node_id]))
            for index, node_id in enumerate(rows[r]):
                position[node_id] = index
    for r, row in enumerate(rows):
        for node_id in row:
            boxes[node_id].layer = r
    return rows


def _f(value: float) -> str:
    return f"{value:.1f}"


def _shape(box: _Box) -> str:
    w, h, x, y = box.w, box.h, box.x, box.y
    style = f'fill="{FILL}" stroke="{LINE}" stroke-width="2"'
    if box.node.shape == "diamond":
        points = (
            f"{_f(x)},{_f(y - h / 2)} {_f(x + w / 2)},{_f(y)} {_f(x)},{_f(y + h / 2)} {_f(x - w / 2)},{_f(y)}"
        )
        return f'<polygon points="{points}" {style}/>'
    rx = 24 if box.node.shape in ("round", "actor") else 6
    return (
        f'<rect x="{_f(x - w / 2)}" y="{_f(y - h / 2)}" width="{_f(w)}" height="{_f(h)}" rx="{rx}" {style}/>'
    )


def _label(box: _Box) -> str:
    first = box.y - (len(box.lines) - 1) * 10.0 + 5.0
    spans = "".join(
        f'<tspan x="{_f(box.x)}" y="{_f(first + 20.0 * i)}">{escape(line)}</tspan>'
        for i, line in enumerate(box.lines)
    )
    return f'<text text-anchor="middle" font-size="16" fill="{INK}" xml:space="preserve">{spans}</text>'


def _group(box: _Box) -> str:
    return f'<g data-node="{escape(box.node.id, quote=True)}">{_shape(box)}{_label(box)}</g>'


def to_svg(diagram: Diagram) -> str:
    """The SVG for a validated diagram."""
    if diagram.kind == "sequence":
        return _sequence_svg(diagram)
    return _layered_svg(diagram)


def _frame(diagram: Diagram, width: float, height: float, body: str) -> str:
    title = escape(diagram.title)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {_f(width)} {_f(height)}" width="{_f(width)}" '
        f'height="{_f(height)}" role="img" aria-label="{escape(diagram.title, quote=True)}"><title>{title}</title>'
        f'<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" '
        f'orient="auto-start-reverse"><path d="M0 0 L10 5 L0 10 z" fill="{LINE}"/></marker></defs>'
        f'<rect x="0" y="0" width="{_f(width)}" height="{_f(height)}" fill="#ffffff"/>{body}</svg>'
    )


def _layered_svg(diagram: Diagram) -> str:
    horizontal = diagram.direction == "LR"
    boxes = {node.id: _Box(node) for node in diagram.nodes}
    for box in boxes.values():
        _size(box)
    rows = _layers(boxes, list(diagram.edges))
    gap_layer, gap_node, margin = 84.0, 44.0, 40.0
    # Along a layer the extent is the sum of the sizes; across layers it is the largest size per layer.
    along = [
        sum((boxes[n].h if horizontal else boxes[n].w) for n in row) + gap_node * (len(row) - 1)
        for row in rows
    ]
    across = [max((boxes[n].w if horizontal else boxes[n].h) for n in row) for row in rows]
    widest = max(along)
    cursor = margin
    for r, row in enumerate(rows):
        centre_across = cursor + across[r] / 2
        start = margin + (widest - along[r]) / 2
        for node_id in row:
            box = boxes[node_id]
            extent = box.h if horizontal else box.w
            centre_along = start + extent / 2
            start += extent + gap_node
            box.x, box.y = (centre_across, centre_along) if horizontal else (centre_along, centre_across)
        cursor += across[r] + gap_layer
    width = (cursor - gap_layer + margin) if horizontal else (widest + 2 * margin)
    height = (widest + 2 * margin) if horizontal else (cursor - gap_layer + margin)
    right_edge = max(b.x + b.w / 2 for b in boxes.values())
    bottom_edge = max(b.y + b.h / 2 for b in boxes.values())
    paths: list[str] = []
    labels: list[str] = []
    for edge in diagram.edges:
        source, target = boxes[edge.source], boxes[edge.target]
        d, mid = _route(source, target, horizontal, (right_edge, bottom_edge))
        paths.append(
            f'<path data-edge="{escape(edge.source, quote=True)}-&gt;{escape(edge.target, quote=True)}" d="{d}" '
            f'fill="none" stroke="{LINE}" stroke-width="2" marker-end="url(#arrow)"/>'
        )
        if edge.label:
            labels.append(
                f'<text x="{_f(mid[0] + 6)}" y="{_f(mid[1] - 6)}" font-size="14" fill="{INK}">{escape(edge.label)}</text>'
            )
    detours = any(boxes[e.target].layer <= boxes[e.source].layer for e in diagram.edges)
    if detours:
        # An edge that goes back (or sideways) runs round the outside of the nodes: leave room for it.
        if horizontal:
            height = max(height, bottom_edge + 36 + margin)
        else:
            width = max(width, right_edge + 36 + margin + 20)
    body = "".join(paths) + "".join(_group(b) for b in boxes.values()) + "".join(labels)
    return _frame(diagram, width, height, body)


def _route(
    source: _Box, target: _Box, horizontal: bool, extent: tuple[float, float]
) -> tuple[str, tuple[float, float]]:
    right_edge, bottom_edge = extent
    if source is target:
        x, y = source.x + source.w / 2, source.y
        d = f"M {_f(x)} {_f(y - 8)} C {_f(x + 50)} {_f(y - 40)}, {_f(x + 50)} {_f(y + 40)}, {_f(x)} {_f(y + 8)}"
        return d, (x + 40, y)
    forward = target.layer > source.layer
    if forward and not horizontal:
        sx, sy, tx, ty = source.x, source.y + source.h / 2, target.x, target.y - target.h / 2
        dy = (ty - sy) / 2
        return (
            f"M {_f(sx)} {_f(sy)} C {_f(sx)} {_f(sy + dy)}, {_f(tx)} {_f(ty - dy)}, {_f(tx)} {_f(ty)}",
            ((sx + tx) / 2, (sy + ty) / 2),
        )
    if forward and horizontal:
        sx, sy, tx, ty = source.x + source.w / 2, source.y, target.x - target.w / 2, target.y
        dx = (tx - sx) / 2
        return (
            f"M {_f(sx)} {_f(sy)} C {_f(sx + dx)} {_f(sy)}, {_f(tx - dx)} {_f(ty)}, {_f(tx)} {_f(ty)}",
            ((sx + tx) / 2, (sy + ty) / 2),
        )
    # A back edge or an edge within a layer goes round the outside: under the nodes when the layers run
    # left to right, down the right-hand side when they run top to bottom.
    if horizontal:
        sx, sy = source.x, source.y + source.h / 2
        tx, ty = target.x, target.y + target.h / 2
        bend = bottom_edge + 36
        return (
            f"M {_f(sx)} {_f(sy)} C {_f(sx)} {_f(bend)}, {_f(tx)} {_f(bend)}, {_f(tx)} {_f(ty)}",
            ((sx + tx) / 2, bend - 4),
        )
    sx, sy = source.x + source.w / 2, source.y
    tx, ty = target.x + target.w / 2, target.y
    bend = right_edge + 36
    return (
        f"M {_f(sx)} {_f(sy)} C {_f(bend)} {_f(sy)}, {_f(bend)} {_f(ty)}, {_f(tx)} {_f(ty)}",
        (bend - 10, (sy + ty) / 2),
    )


def _sequence_svg(diagram: Diagram) -> str:
    margin, step, top = 40.0, 200.0, 40.0
    boxes = {node.id: _Box(node) for node in diagram.nodes}
    for index, box in enumerate(boxes.values()):
        _size(box)
        box.x = margin + 80 + index * step
        box.y = top + 32
    height = top + 64 + 60 + 56 * max(len(diagram.edges), 1) + 40
    parts: list[str] = []
    for box in boxes.values():
        parts.append(
            f'<line x1="{_f(box.x)}" y1="{_f(box.y + box.h / 2)}" x2="{_f(box.x)}" y2="{_f(height - 20)}" '
            f'stroke="{LINE}" stroke-width="2" stroke-dasharray="6 6"/>'
        )
    for index, edge in enumerate(diagram.edges):
        y = top + 64 + 60 + 56 * index
        a, b = boxes[edge.source], boxes[edge.target]
        if a is b:
            d = f"M {_f(a.x)} {_f(y - 10)} L {_f(a.x + 50)} {_f(y - 10)} L {_f(a.x + 50)} {_f(y + 14)} L {_f(a.x + 4)} {_f(y + 14)}"
            mid = (a.x + 56, y)
        else:
            direction = 1 if b.x > a.x else -1
            d = f"M {_f(a.x)} {_f(y)} L {_f(b.x - direction * 2)} {_f(y)}"
            mid = ((a.x + b.x) / 2 - 20, y - 8)
        parts.append(
            f'<path data-edge="{escape(edge.source, quote=True)}-&gt;{escape(edge.target, quote=True)}" d="{d}" '
            f'fill="none" stroke="{LINE}" stroke-width="2" marker-end="url(#arrow)"/>'
        )
        if edge.label:
            parts.append(
                f'<text x="{_f(mid[0])}" y="{_f(mid[1] - 4)}" font-size="14" fill="{INK}">{escape(edge.label)}</text>'
            )
    parts.extend(_group(b) for b in boxes.values())
    width = margin * 2 + 160 + step * (len(boxes) - 1)
    return _frame(diagram, width, height, "".join(parts))
