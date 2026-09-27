#!/usr/bin/env python3
"""Line coverage of the Python service modules, standard library only (QA-001).

Runs the `python/` package's own test suite in-process under a `sys.monitoring` (PEP 669)
line tracer and reports, per module, the executable lines a test reached. Executable lines are
the lines the compiler attributes to bytecode (`co_lines()` over every nested code object), so
comments, blank lines and bare docstrings of functions that never run are handled the same way
coverage tools do: a `def` or `class` line executes when its module is imported; a body line
executes only when called.

  uv run --project python python scripts/ci/pycoverage.py --out coverage-python.json [-- pytest args]

Each line location is disabled after its first hit (`sys.monitoring.DISABLE`), so the tracer
costs one callback per line, not per execution.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import types
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import TypeVar

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / "python" / "intelligence"
#: Code a person does not write: generated contract bindings are checked by the contract suite.
EXCLUDED = ("contracts/generated/",)
TOOL = sys.monitoring.COVERAGE_ID
T = TypeVar("T")


def measured(path: Path, package: Path = PACKAGE) -> bool:
    relative = path.relative_to(package).as_posix()
    return not any(relative.startswith(prefix) for prefix in EXCLUDED)


def code_objects(code: types.CodeType) -> Iterator[types.CodeType]:
    yield code
    for constant in code.co_consts:
        if isinstance(constant, types.CodeType):
            yield from code_objects(constant)


def executable_lines(path: Path) -> set[int]:
    """Lines the compiler attributes to bytecode, as the interpreter will report them."""
    code = compile(path.read_text(), str(path), "exec", dont_inherit=True)
    lines: set[int] = set()
    for nested in code_objects(code):
        for _, _, line in nested.co_lines():
            if line is not None and line > 0:
                lines.add(line)
    return lines


def trace(body: Callable[[], T], package: Path = PACKAGE) -> tuple[T, dict[str, set[int]]]:
    """Run `body` with the line tracer on; return its result and the lines reached under `package`."""
    hits: dict[str, set[int]] = {}
    prefix = str(package) + os.sep

    def on_line(code: types.CodeType, line: int) -> object:
        filename = code.co_filename
        if filename.startswith(prefix):
            hits.setdefault(filename, set()).add(line)
        return sys.monitoring.DISABLE

    sys.monitoring.use_tool_id(TOOL, "quansio-pycoverage")
    sys.monitoring.register_callback(TOOL, sys.monitoring.events.LINE, on_line)
    sys.monitoring.set_events(TOOL, sys.monitoring.events.LINE)
    try:
        result = body()
    finally:
        sys.monitoring.set_events(TOOL, 0)
        sys.monitoring.register_callback(TOOL, sys.monitoring.events.LINE, None)
        sys.monitoring.free_tool_id(TOOL)
    return result, hits


def run(pytest_args: list[str]) -> tuple[int, dict[str, set[int]]]:
    """The `python/` package's own test suite, traced."""

    def suite() -> int:
        import pytest

        os.chdir(ROOT / "python")
        return int(pytest.main(["-q", "-p", "no:cacheprovider", *pytest_args]))

    return trace(suite)


def report(hits: dict[str, set[int]], package: Path = PACKAGE) -> dict[str, object]:
    modules = []
    total_lines = total_hit = 0
    for path in sorted(package.rglob("*.py")):
        if not measured(path, package):
            continue
        lines = executable_lines(path)
        reached = lines & hits.get(str(path), set())
        total_lines += len(lines)
        total_hit += len(reached)
        modules.append(
            {
                "path": path.relative_to(package.parent).as_posix(),
                "lines": len(lines),
                "covered": len(reached),
                "percent": round(100 * len(reached) / len(lines), 2) if lines else 100.0,
            }
        )
    return {
        "tool": "sys.monitoring line tracer (scripts/ci/pycoverage.py)",
        "scope": f"{package.name} excluding {list(EXCLUDED)}",
        "lines": total_lines,
        "covered": total_hit,
        "percent": round(100 * total_hit / total_lines, 2) if total_lines else 100.0,
        "modules": modules,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("pytest_args", nargs="*")
    args = parser.parse_args(argv)
    status, hits = run(args.pytest_args)
    summary = {"pytest_exit": status, **report(hits)}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(summary, indent=2) + "\n")
    print(f"python coverage: {summary['percent']}% of {summary['lines']} lines (pytest exit {status})")
    return status


if __name__ == "__main__":
    raise SystemExit(main())
