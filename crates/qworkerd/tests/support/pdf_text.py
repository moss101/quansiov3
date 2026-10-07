#!/usr/bin/env python3
"""An independent PDF text reader for the render tests (ADP-006).

It knows nothing about the browser that printed the file: it walks the file's objects, inflates the
content streams with zlib, reads each font's ToUnicode map and prints the text it finds, one page
at a time, separated by a form feed. It exists so a test can say what a PDF *contains* without
trusting the renderer's own account of it. Standard library only.

    python3 pdf_text.py file.pdf
"""

from __future__ import annotations

import re
import sys
import zlib

OBJ = re.compile(rb"(\d+)\s+(\d+)\s+obj\b(.*?)\bendobj", re.S)


def objects(data: bytes) -> dict[int, bytes]:
    found: dict[int, bytes] = {}
    for match in OBJ.finditer(data):
        found[int(match.group(1))] = match.group(3)
    return found


def stream_of(body: bytes) -> bytes | None:
    start = body.find(b"stream")
    if start < 0:
        return None
    start += len(b"stream")
    if body[start : start + 2] == b"\r\n":
        start += 2
    elif body[start : start + 1] in (b"\n", b"\r"):
        start += 1
    end = body.rfind(b"endstream")
    raw = body[start:end]
    if b"/FlateDecode" in body[: body.find(b"stream")]:
        try:
            return zlib.decompress(raw)
        except zlib.error:
            return zlib.decompressobj().decompress(raw)
    return raw


def refs(text: bytes) -> list[int]:
    return [int(n) for n in re.findall(rb"(\d+)\s+0\s+R", text)]


def parse_cmap(data: bytes) -> dict[int, str]:
    mapping: dict[int, str] = {}
    for block in re.findall(rb"beginbfchar(.*?)endbfchar", data, re.S):
        for source, target in re.findall(rb"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>", block):
            mapping[int(source, 16)] = bytes.fromhex(target.decode()).decode("utf-16-be", "replace")
    for block in re.findall(rb"beginbfrange(.*?)endbfrange", data, re.S):
        for low, high, target in re.findall(rb"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>", block):
            first = int(target, 16)
            for offset, code in enumerate(range(int(low, 16), int(high, 16) + 1)):
                mapping[code] = chr(first + offset)
        for low, high, array in re.findall(rb"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*\[(.*?)\]", block, re.S):
            for offset, item in enumerate(re.findall(rb"<([0-9A-Fa-f]+)>", array)):
                mapping[int(low, 16) + offset] = bytes.fromhex(item.decode()).decode("utf-16-be", "replace")
    return mapping


def font_maps(objs: dict[int, bytes]) -> dict[int, tuple[int, dict[int, str]]]:
    """Per font object: the code width in bytes (2 for an Identity-H composite font) and its map."""
    maps: dict[int, tuple[int, dict[int, str]]] = {}
    for number, body in objs.items():
        if b"/Type /Font" not in body and b"/Type/Font" not in body:
            continue
        match = re.search(rb"/ToUnicode\s+(\d+)\s+0\s+R", body)
        if match and int(match.group(1)) in objs:
            data = stream_of(objs[int(match.group(1))])
            if data:
                width = 2 if b"/Identity-H" in body else 1
                maps[number] = (width, parse_cmap(data))
    return maps


def pages(objs: dict[int, bytes]) -> list[int]:
    ordered: list[int] = []

    def walk(number: int) -> None:
        body = objs.get(number, b"")
        if re.search(rb"/Type\s*/Pages", body):
            kids = re.search(rb"/Kids\s*\[(.*?)\]", body, re.S)
            for kid in refs(kids.group(1)) if kids else []:
                walk(kid)
        elif re.search(rb"/Type\s*/Page\b", body):
            ordered.append(number)

    for number, body in objs.items():
        if re.search(rb"/Type\s*/Pages", body) and b"/Parent" not in body:
            walk(number)
    return ordered


def resources_fonts(objs: dict[int, bytes], page_body: bytes) -> dict[bytes, int]:
    body = page_body
    match = re.search(rb"/Resources\s+(\d+)\s+0\s+R", body)
    if match:
        body = objs.get(int(match.group(1)), b"")
    font = re.search(rb"/Font\s*<<(.*?)>>", body, re.S)
    if font:
        return {name: int(n) for name, n in re.findall(rb"(/F\d+)\s+(\d+)\s+0\s+R", font.group(1))}
    font_ref = re.search(rb"/Font\s+(\d+)\s+0\s+R", body)
    if font_ref:
        inner = objs.get(int(font_ref.group(1)), b"")
        return {name: int(n) for name, n in re.findall(rb"(/F\d+)\s+(\d+)\s+0\s+R", inner)}
    return {}


def decode_hex_string(token: bytes, font: tuple[int, dict[int, str]]) -> str:
    width, mapping = font
    raw = bytes.fromhex(re.sub(rb"\s+", b"", token).decode())
    out = []
    for i in range(0, len(raw) - width + 1, width):
        code = int.from_bytes(raw[i : i + width], "big")
        out.append(mapping.get(code, chr(code) if width == 1 else ""))
    return "".join(out)


def page_text(objs: dict[int, bytes], fonts: dict[int, tuple[int, dict[int, str]]], number: int) -> str:
    body = objs[number]
    named = resources_fonts(objs, body)
    contents = re.search(rb"/Contents\s*(\[(.*?)\]|(\d+)\s+0\s+R)", body, re.S)
    streams: list[bytes] = []
    if contents:
        for ref in refs(contents.group(1)):
            data = stream_of(objs.get(ref, b""))
            if data:
                streams.append(data)
    text: list[str] = []
    current: tuple[int, dict[int, str]] = (1, {})
    for stream in streams:
        last_y = None
        for token in re.finditer(
            rb"(/F\d+)\s+[\d.]+\s+Tf|(-?[\d.]+)\s+(-?[\d.]+)\s+Td|(-?[\d.]+\s+){5}(-?[\d.]+)\s+Tm|\[(.*?)\]\s*TJ|<([0-9A-Fa-f\s]*)>\s*Tj",
            stream,
            re.S,
        ):
            if token.group(1):
                current = fonts.get(named.get(token.group(1), -1), (1, {}))
            elif token.group(6) is not None:
                for piece in re.finditer(rb"<([0-9A-Fa-f\s]*)>|\(((?:\\.|[^\\)])*)\)", token.group(6)):
                    if piece.group(1) is not None:
                        text.append(decode_hex_string(piece.group(1), current))
                    else:
                        text.append(piece.group(2).decode("latin-1"))
            elif token.group(7) is not None:
                text.append(decode_hex_string(token.group(7), current))
            elif token.group(3) is not None:
                y = float(token.group(3))
                if y != 0:
                    text.append("\n")
            elif token.group(5) is not None:
                y = token.group(5)
                if last_y is not None and y != last_y:
                    text.append("\n")
                last_y = y
        text.append("\n")
    return "".join(text)


def main() -> int:
    data = open(sys.argv[1], "rb").read()
    objs = objects(data)
    fonts = font_maps(objs)
    sheets = [page_text(objs, fonts, number) for number in pages(objs)]
    sys.stdout.write("\f".join(sheets))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
