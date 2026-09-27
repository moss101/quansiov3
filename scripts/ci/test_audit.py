#!/usr/bin/env python3
"""Audit what the pipeline's test runs actually skipped (QA-001).

A release-blocking test may not run only as the documented real-boundary convention: it names
the absent `QUANSIO_TEST_*` variable and reports `BLOCKED_EXTERNAL`. This reads the logs the
baseline pipeline wrote for its test gates and fails on anything else that did not run:

* pytest (run with `-rsxX`, which `ci_summary.py` sets): a SKIPPED reason that names neither
  `BLOCKED_EXTERNAL` nor a `QUANSIO_TEST_*` variable; any XFAIL or XPASS;
* cargo test: any ignored test;
* vitest (each package's JSON report, written under the pipeline): a skipped test in a file
  that names no `BLOCKED_EXTERNAL` reason; any todo;
* XCTest: a skip whose message does not say `BLOCKED_EXTERNAL`.

It also publishes the inventory of what did not run and why, so a reader sees the boundaries a
run could not reach without reading every log.

  python3.12 scripts/ci/test_audit.py [--logs <dir>] [--out <inventory.json>]

`--logs` defaults to `$QUANSIO_CI_LOG_DIR` (set by `ci_summary.py` for its gates).

Python 3.11+ ; standard library only.
"""
from __future__ import annotations

import argparse
import json
import os
import re
from pathlib import Path
from typing import Dict, List, Optional, Sequence

#: The pipeline gates that run tests, by the log `ci_summary.py` writes for each.
TEST_LOGS = ("toolchains.log", "tests.log")

ENV_VARIABLE = re.compile(r"QUANSIO_TEST_[A-Z0-9_]+")
PYTEST_SKIPPED = re.compile(r"^SKIPPED \[(\d+)\] (\S+?):(\d+): (.*)$")
PYTEST_XFAIL = re.compile(r"^(XFAIL|XPASS) (\S+)(?: - (.*))?$")
PYTEST_TOTALS = re.compile(r"(\d+) (passed|failed|skipped|xfailed|xpassed|errors?)")
CARGO_RESULT = re.compile(r"^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored;")
CARGO_RUNNING = re.compile(r"^\s*Running (\S+) \(")
VITEST_TESTS = re.compile(r"^\s*(\S+ test: )?\s*Tests\s+(.*)\((\d+)\)\s*$")
VITEST_COUNT = re.compile(r"(\d+) (passed|failed|skipped|todo)")
XCTEST_SKIP = re.compile(r"-\[(\S+) (\S+)\] : Test skipped - (.*)$")


def blocked_reason(reason: str) -> bool:
    return "BLOCKED_EXTERNAL" in reason or ENV_VARIABLE.search(reason) is not None


def audit_text(name: str, text: str) -> Dict[str, List[dict]]:
    """Findings and the blocked inventory for one gate's log."""
    findings: List[dict] = []
    blocked: List[dict] = []
    pytest_reported = 0
    pytest_skipped_total = 0
    vitest_not_run = 0
    binary = None
    for raw in text.splitlines():
        line = raw.rstrip()
        if match := PYTEST_SKIPPED.match(line):
            count, path, lineno, reason = int(match[1]), match[2], match[3], match[4]
            pytest_reported += count
            row = {"runner": "pytest", "log": name, "test": f"{path}:{lineno}", "count": count, "reason": reason}
            (blocked if blocked_reason(reason) else findings).append(row)
        elif match := PYTEST_XFAIL.match(line):
            findings.append({"runner": "pytest", "log": name, "test": match[2], "count": 1,
                             "reason": f"{match[1]} is banned in release-blocking suites: {match[3] or ''}".strip()})
        elif line.startswith("=") or re.match(r"^\d+ (passed|failed)", line):
            totals = dict((kind, int(n)) for n, kind in PYTEST_TOTALS.findall(line))
            if "passed" in totals or "skipped" in totals:
                pytest_skipped_total += totals.get("skipped", 0)
                for kind in ("xfailed", "xpassed"):
                    if totals.get(kind):
                        findings.append({"runner": "pytest", "log": name, "test": "(run totals)",
                                         "count": totals[kind], "reason": f"{kind} is banned"})
        elif match := CARGO_RUNNING.match(line):
            binary = match[1]
        elif match := CARGO_RESULT.match(line):
            ignored = int(match[3])
            if ignored:
                findings.append({"runner": "cargo", "log": name, "test": binary or "(unknown)",
                                 "count": ignored, "reason": "ignored tests are banned"})
        elif match := VITEST_TESTS.match(line):
            counts = dict((kind, int(n)) for n, kind in VITEST_COUNT.findall(match[2]))
            vitest_not_run += counts.get("skipped", 0) + counts.get("todo", 0)
        elif match := XCTEST_SKIP.search(line):
            row = {"runner": "xctest", "log": name, "test": f"{match[1]}.{match[2]}", "count": 1, "reason": match[3]}
            (blocked if "BLOCKED_EXTERNAL" in match[3] else findings).append(row)
    # pytest -q without -rs prints totals but no reasons: an unexplained count is a finding.
    if pytest_skipped_total > pytest_reported:
        findings.append({"runner": "pytest", "log": name, "test": "(run totals)",
                         "count": pytest_skipped_total - pytest_reported,
                         "reason": "skips without a reported reason (run pytest with -rsxX)"})
    return {"findings": findings, "blocked": blocked, "vitest_not_run": vitest_not_run}


def audit_vitest(name: str, report: dict) -> Dict[str, List[dict]]:
    """One vitest JSON report: a test file may leave tests unrun only as a named block.

    The convention (`describe.skipIf(blocked !== null)` beside `it.skip(blocked, ...)`) reports a
    file's reason once, as the title of a skipped test; every other skipped test in that file is
    covered by it. A `todo` is never a reason.
    """
    findings: List[dict] = []
    blocked: List[dict] = []
    for result in report.get("testResults", []):
        test_file = result.get("name", "(unknown)")
        unrun = [a for a in result.get("assertionResults", []) if a.get("status") in ("skipped", "pending", "todo")]
        if not unrun:
            continue
        reasons = [a["title"] for a in unrun if a.get("status") != "todo" and blocked_reason(a.get("title", ""))]
        todos = [a for a in unrun if a.get("status") == "todo"]
        for todo in todos:
            findings.append({"runner": "vitest", "log": name, "test": f"{test_file} > {todo.get('fullName')}",
                             "count": 1, "reason": "todo tests are banned"})
        rest = len(unrun) - len(todos)
        if not rest:
            continue
        row = {"runner": "vitest", "log": name, "test": test_file, "count": rest,
               "reason": reasons[0] if reasons else "skipped without a BLOCKED_EXTERNAL reason"}
        (blocked if reasons else findings).append(row)
    return {"findings": findings, "blocked": blocked}


def audit(log_dir: Path, logs: Sequence[str] = TEST_LOGS) -> dict:
    findings: List[dict] = []
    blocked: List[dict] = []
    vitest_not_run = 0
    for name in logs:
        path = log_dir / name
        if not path.is_file():
            findings.append({"runner": "-", "log": name, "test": "-", "count": 0,
                             "reason": f"{path} is missing: the gate did not run or wrote no log"})
            continue
        result = audit_text(name, path.read_text(errors="replace"))
        findings += result["findings"]
        blocked += result["blocked"]
        vitest_not_run += result["vitest_not_run"]
    reports = sorted(log_dir.glob("vitest-*.json"))
    explained = 0
    for path in reports:
        try:
            report = json.loads(path.read_text())
        except json.JSONDecodeError as error:
            findings.append({"runner": "vitest", "log": path.name, "test": "-", "count": 0,
                             "reason": f"unreadable report: {error}"})
            continue
        result = audit_vitest(path.name, report)
        findings += result["findings"]
        blocked += result["blocked"]
        explained += sum(row["count"] for row in result["findings"] + result["blocked"])
    # The text summaries count what vitest did not run; the JSON reports say why. A count the
    # reports do not account for (a package without a report) has no reason anywhere.
    if vitest_not_run > explained:
        findings.append({"runner": "vitest", "log": ",".join(logs), "test": "(run totals)",
                         "count": vitest_not_run - explained,
                         "reason": "skipped or todo tests with no JSON report giving a reason"})
    by_variable: Dict[str, int] = {}
    for row in blocked:
        for variable in sorted(set(ENV_VARIABLE.findall(row["reason"]))) or ["(unnamed)"]:
            by_variable[variable] = by_variable.get(variable, 0) + row["count"]
    return {
        "status": "FAIL" if findings else "PASS",
        "findings": findings,
        "blocked_external": blocked,
        "blocked_by_variable": dict(sorted(by_variable.items())),
    }


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Audit skipped tests in the pipeline's logs")
    parser.add_argument("--logs", type=Path, default=None)
    parser.add_argument("--out", type=Path, default=None)
    args = parser.parse_args(argv)
    log_dir = args.logs or (Path(os.environ["QUANSIO_CI_LOG_DIR"]) if os.environ.get("QUANSIO_CI_LOG_DIR") else None)
    if log_dir is None:
        parser.error("--logs or QUANSIO_CI_LOG_DIR is required")
    report = audit(log_dir)
    out = args.out or log_dir / "test-audit.json"
    out.write_text(json.dumps(report, indent=2) + "\n")
    for row in report["findings"]:
        print(f"FINDING {row['runner']} {row['test']} x{row['count']}: {row['reason']}")
    for variable, count in report["blocked_by_variable"].items():
        print(f"BLOCKED_EXTERNAL {variable}: {count} test(s) did not run")
    print(f"test audit: {report['status']} ({out})")
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
