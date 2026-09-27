"""QA-001's pipeline tools: the test audit and the coverage measurement against DOSSIER §21.4.

Each tool is exercised on fixtures shaped exactly like what the real runners print (pytest with
`-rsxX`, cargo's libtest, vitest's JSON reporter, XCTest), and on the failure paths it exists
for: an unexplained skip, an xfail, an ignored Rust test, a todo, a coverage floor missed, a
safety-critical file no test maps.
"""

from __future__ import annotations

import json
import textwrap
from pathlib import Path

import pytest

from scripts.ci import ci_summary, coverage, pycoverage, test_audit

PYTEST_BLOCKED = (
    "SKIPPED [2] tests/security/test_x.py:30: BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set"
)


def write_logs(tmp_path: Path, toolchains: str, tests: str = "1 passed in 0.1s\n") -> Path:
    (tmp_path / "toolchains.log").write_text(textwrap.dedent(toolchains))
    (tmp_path / "tests.log").write_text(textwrap.dedent(tests))
    return tmp_path


def vitest_report(tmp_path: Path, name: str, *files: tuple[str, list[tuple[str, str]]]) -> None:
    results = [
        {
            "name": test_file,
            "assertionResults": [{"title": title, "fullName": title, "status": status} for title, status in cases],
        }
        for test_file, cases in files
    ]
    (tmp_path / f"vitest-{name}.json").write_text(json.dumps({"testResults": results}))


# ------------------------------------------------------------------------------ test audit


def test_a_run_whose_only_skips_are_named_blocks_passes(tmp_path: Path) -> None:
    logs = write_logs(
        tmp_path,
        f"""\
        test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s
        {PYTEST_BLOCKED}
        628 passed, 2 skipped in 41.11s
        apps/web test:       Tests  13 passed | 4 skipped (17)
        /x/ComputerUseTests.swift:17: -[Bridge.ComputerUseTests testTree] : Test skipped - BLOCKED_EXTERNAL: QUANSIO_TEST_MACOS_COMPUTER_USE=1 is required
        """,
    )
    vitest_report(
        logs,
        "web",
        ("tests/web.e2e.test.ts", [("journey", "skipped"), ("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set", "skipped")] * 2),
        ("tests/unit.test.ts", [("renders", "passed")]),
    )
    report = test_audit.audit(logs)
    assert report["status"] == "PASS", report["findings"]
    assert report["blocked_by_variable"] == {
        "QUANSIO_TEST_MACOS_COMPUTER_USE": 1,
        "QUANSIO_TEST_POSTGRES_URL": 6,
    }


def test_a_pytest_skip_without_a_named_boundary_fails(tmp_path: Path) -> None:
    logs = write_logs(
        tmp_path,
        """\
        SKIPPED [1] tests/test_flaky.py:9: flaky on CI
        1 passed, 1 skipped in 0.2s
        """,
    )
    report = test_audit.audit(logs)
    assert report["status"] == "FAIL"
    assert report["findings"][0]["reason"] == "flaky on CI"


def test_skips_with_no_reported_reason_fail(tmp_path: Path) -> None:
    """pytest `-q` without `-rs` prints a count and no reason: that count is unexplained."""
    report = test_audit.audit(write_logs(tmp_path, "10 passed, 3 skipped in 1.0s\n"))
    assert report["status"] == "FAIL"
    assert report["findings"][0]["count"] == 3


@pytest.mark.parametrize(
    "line",
    [
        "XFAIL tests/test_a.py::test_known - not fixed yet",
        "XPASS tests/test_a.py::test_known",
        "5 passed, 1 xfailed in 0.3s",
    ],
)
def test_xfail_and_xpass_are_banned(tmp_path: Path, line: str) -> None:
    assert test_audit.audit(write_logs(tmp_path, line + "\n"))["status"] == "FAIL"


def test_an_ignored_rust_test_fails_and_names_its_binary(tmp_path: Path) -> None:
    logs = write_logs(
        tmp_path,
        """\
             Running tests/effects.rs (target/debug/deps/effects-0123456789abcdef)
        test result: ok. 11 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.2s
        """,
    )
    report = test_audit.audit(logs)
    assert report["status"] == "FAIL"
    assert report["findings"][0]["test"] == "tests/effects.rs"


def test_vitest_skips_need_a_named_block_in_their_file_and_todos_are_banned(tmp_path: Path) -> None:
    logs = write_logs(tmp_path, "apps/desktop test:       Tests  3 passed | 2 skipped | 1 todo (6)\n")
    vitest_report(
        logs,
        "desktop",
        ("tests/a.test.ts", [("wip", "skipped"), ("later", "skipped"), ("draft", "todo")]),
    )
    report = test_audit.audit(logs)
    reasons = sorted(row["reason"] for row in report["findings"])
    assert reasons == ["skipped without a BLOCKED_EXTERNAL reason", "todo tests are banned"]


def test_vitest_skips_with_no_report_are_unexplained(tmp_path: Path) -> None:
    report = test_audit.audit(write_logs(tmp_path, "sdk/typescript test:       Tests  8 passed | 1 skipped (9)\n"))
    assert report["status"] == "FAIL"
    assert "no JSON report" in report["findings"][0]["reason"]


def test_an_xctest_skip_must_say_blocked_external(tmp_path: Path) -> None:
    logs = write_logs(tmp_path, "/x/A.swift:3: -[Bridge.A testB] : Test skipped - not ready\n")
    assert test_audit.audit(logs)["status"] == "FAIL"


def test_a_missing_test_log_fails_closed(tmp_path: Path) -> None:
    (tmp_path / "toolchains.log").write_text("1 passed in 0.1s\n")
    report = test_audit.audit(tmp_path)
    assert report["status"] == "FAIL"
    assert "tests.log" in report["findings"][0]["reason"]


def test_the_pipeline_reports_skip_reasons_and_audits_last() -> None:
    names = [name for name, _ in ci_summary.GATES]
    assert names[-1] == "test-audit"
    assert names.index("toolchains") < names.index("test-audit")
    assert names.index("tests") < names.index("test-audit")


def test_the_pipeline_hands_its_log_directory_and_pytest_reasons_to_the_gates(tmp_path: Path) -> None:
    summary = ci_summary.run(
        commands=['python3.12 -c "import os; print(os.environ[\'QUANSIO_CI_LOG_DIR\']); print(os.environ[\'PYTEST_ADDOPTS\'])"'],
        out=tmp_path / "summary.json",
        root=ci_summary.ROOT,
    )
    tail = summary["results"][0]["tail"]
    assert str(tmp_path / "logs") in tail
    assert "-rsxX" in tail


# ------------------------------------------------------------------------------ coverage

EXPECTATIONS = {
    "source": "fixture",
    "rust": {
        "exclude": [],
        "safety_critical": {"line_min": 90, "branch": "reported", "areas": {"effects": ["crates/server/src/effects/"]}},
        "other": {"line_min": 75},
    },
    "python": {"line_min": 80},
    "typescript": {"line_min": 70, "status": "BLOCKED_EXTERNAL", "reason": "fixture"},
}


def row(lines: int, covered: int, branches: int = 0, branches_covered: int = 0) -> dict:
    return {"lines": lines, "lines_covered": covered, "branches": branches, "branches_covered": branches_covered}


def rust_measurement(**files: dict) -> dict:
    return {"cargo_test_exit": 0, "files": files, "unreportable": [], "unmapped": []}


PYTHON_OK = {"pytest_exit": 0, "lines": 100, "covered": 85, "percent": 85.0}


def test_coverage_at_its_floors_passes_and_reports_branches() -> None:
    rust = rust_measurement(
        **{
            "crates/server/src/effects/ledger.rs": row(100, 95, 10, 7),
            "crates/core/src/lib.rs": row(100, 80),
        }
    )
    report = coverage.check(rust, PYTHON_OK, EXPECTATIONS)
    assert report["status"] == "PASS", report["findings"]
    assert report["groups"]["effects"]["line_percent"] == 95.0
    assert report["groups"]["effects"]["branch_percent"] == 70.0
    assert report["groups"]["rust-other"]["line_percent"] == 80.0


@pytest.mark.parametrize(
    ("rust", "python", "finding"),
    [
        (rust_measurement(**{"crates/server/src/effects/ledger.rs": row(100, 89)}), PYTHON_OK, "effects: 89.0% lines < 90%"),
        (
            rust_measurement(**{"crates/server/src/effects/ledger.rs": row(10, 10), "crates/core/src/lib.rs": row(100, 70)}),
            PYTHON_OK,
            "other Rust: 70.0% lines < 75%",
        ),
        (rust_measurement(**{"crates/core/src/lib.rs": row(10, 10)}), PYTHON_OK, "no measured file"),
        (
            rust_measurement(**{"crates/server/src/effects/ledger.rs": row(10, 10)}),
            {**PYTHON_OK, "percent": 79.9},
            "python: 79.9% lines < 80%",
        ),
        (rust_measurement(**{"crates/server/src/effects/ledger.rs": row(10, 10)}), None, "python: no measurement"),
        (
            {**rust_measurement(**{"crates/server/src/effects/ledger.rs": row(10, 10)}), "cargo_test_exit": 101},
            PYTHON_OK,
            "cargo test exited 101",
        ),
        (
            {**rust_measurement(**{"crates/server/src/effects/ledger.rs": row(10, 10)}), "unmapped": ["crates/server/src/effects/new.rs"]},
            PYTHON_OK,
            "no test binary maps",
        ),
        (
            {**rust_measurement(**{"crates/server/src/effects/ledger.rs": row(10, 10)}), "unreportable": ["crates/server/src/effects/odd.rs"]},
            PYTHON_OK,
            "could not export",
        ),
    ],
)
def test_coverage_fails_closed(rust: dict, python: dict | None, finding: str) -> None:
    report = coverage.check(rust, python, EXPECTATIONS)
    assert report["status"] == "FAIL"
    assert any(finding in text for text in report["findings"]), report["findings"]


def test_the_longest_matching_area_owns_a_file() -> None:
    areas = {"runtime": ["crates/server/src/runtime/"], "recovery": ["crates/server/src/runtime/recovery/"]}
    assert coverage.group_of("crates/server/src/runtime/recovery/point.rs", areas) == "recovery"
    assert coverage.group_of("crates/server/src/runtime/mod.rs", areas) == "runtime"
    assert coverage.group_of("crates/core/src/lib.rs", areas) is None


def test_the_expectations_are_dossier_21_4_and_name_real_paths() -> None:
    expectations = coverage.load_expectations()
    assert expectations["rust"]["safety_critical"]["line_min"] == 90
    assert expectations["rust"]["other"]["line_min"] == 75
    assert expectations["python"]["line_min"] == 80
    assert expectations["typescript"]["line_min"] == 70
    areas = expectations["rust"]["safety_critical"]["areas"]
    assert set(areas) >= {"capability", "policy", "effects", "runtime", "recovery", "secrets", "egress", "qworkerd-protocol"}
    for prefixes in areas.values():
        for prefix in prefixes:
            assert (coverage.ROOT / prefix).exists(), prefix


def test_the_bisecting_export_isolates_the_files_llvm_cov_crashes_on(monkeypatch: pytest.MonkeyPatch) -> None:
    crashing = {"crates/a/src/lib.rs", "crates/c/src/x.rs"}

    def export(_cov: Path, _prof: Path, _objects: list[str], sources: list[str]) -> list[dict] | None:
        if crashing & set(sources):
            return None
        return [{"filename": source} for source in sources]

    monkeypatch.setattr(coverage, "export", export)
    sources = ["crates/a/src/lib.rs", "crates/b/src/y.rs", "crates/c/src/x.rs", "crates/d/src/z.rs", "crates/e/src/w.rs"]
    files, failed = coverage.export_bisecting(Path("cov"), Path("prof"), ["obj"], sources)
    assert sorted(failed) == sorted(crashing)
    assert sorted(entry["filename"] for entry in files) == ["crates/b/src/y.rs", "crates/d/src/z.rs", "crates/e/src/w.rs"]


# ------------------------------------------------------------------------------ python tracer


def test_the_tracer_counts_the_lines_a_call_reached(tmp_path: Path) -> None:
    package = tmp_path / "fixturepkg"
    package.mkdir()
    (package / "__init__.py").write_text("")
    (package / "mod.py").write_text(
        textwrap.dedent(
            """\
            def reached(flag):
                if flag:
                    return "yes"
                return "no"


            def never():
                value = 1
                return value
            """
        )
    )
    import importlib.util

    spec = importlib.util.spec_from_file_location("fixturepkg_mod", package / "mod.py")
    assert spec and spec.loader

    def body() -> str:
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)  # type: ignore[union-attr]
        return module.reached(True)

    result, hits = pycoverage.trace(body, package)
    assert result == "yes"
    summary = pycoverage.report(hits, package)
    module = next(row for row in summary["modules"] if row["path"].endswith("mod.py"))
    # Executable: both `def` lines, `if flag`, both returns in `reached`, both lines of `never`.
    assert module["lines"] == 7
    # Reached: the two `def` lines on import, `if flag` and `return "yes"`.
    assert module["covered"] == 4
