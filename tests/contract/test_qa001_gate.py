"""QA-001: release-blocking suites carry no unexplained skips or xfails.

A skip is allowed only as the documented real-boundary convention: a marker string
naming the `QUANSIO_TEST_*` variable that was absent (`BLOCKED_EXTERNAL`). Any other
skip/xfail in a release-blocking suite is a finding.
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

#: Suites that gate the release. Everything here must run to green on a clean checkout.
RELEASE_BLOCKING_DIRS = (
    ROOT / "tests" / "contract",
    ROOT / "tests" / "evaluation",
    ROOT / "tests" / "architecture",
    ROOT / "tests" / "ci",
    ROOT / "tests" / "performance",
    ROOT / "tests" / "recovery",
    ROOT / "tests" / "security",
)

ENV_SKIP = re.compile(r"QUANSIO_TEST_[A-Z0-9_]+")


def test_release_blocking_suites_have_no_unexplained_skips() -> None:
    """Named test: skipped/xfail release-blocking tests are banned."""
    offenders: list[str] = []
    gate_self = Path(__file__).resolve()
    for directory in RELEASE_BLOCKING_DIRS:
        for path in sorted(directory.rglob("test_*.py")):
            if path.resolve() == gate_self:
                continue  # this scanner quotes the patterns it bans
            text = path.read_text()
            for lineno, line in enumerate(text.splitlines(), start=1):
                if "pytest.mark.skip" in line or "pytest.mark.xfail" in line or "xfail(" in line:
                    # A conditional skip is legal only when it names the absent
                    # QUANSIO_TEST_* variable (the documented real-boundary convention).
                    context = "\n".join(text.splitlines()[max(0, lineno - 4) : lineno + 2])
                    if not ENV_SKIP.search(context):
                        rel = path.relative_to(ROOT)
                        offenders.append(f"{rel}:{lineno}: {line.strip()[:100]}")
    assert offenders == [], f"unexplained skips in release-blocking suites: {offenders}"


def test_rust_release_suites_have_no_ignore_annotations() -> None:
    """The same ban for the Rust integration suites (`#[ignore]`)."""
    offenders: list[str] = []
    for path in sorted((ROOT / "crates").rglob("tests/**/*.rs")):
        text = path.read_text()
        for lineno, line in enumerate(text.splitlines(), start=1):
            if "#[ignore" in line:
                offenders.append(f"{path.relative_to(ROOT)}:{lineno}")
    assert offenders == [], f"ignored Rust tests: {offenders}"


def test_contract_suite_itself_is_strict() -> None:
    """The schema/contract suite runs against the repository's real artifacts."""
    from tests.contract import test_contracts  # noqa: F401  (import proves it loads)

    assert (ROOT / "schemas" / "openapi" / "public-api-v1.yaml").is_file()
    assert (ROOT / "schemas" / "json" / "evidence-summary.schema.json").is_file()
