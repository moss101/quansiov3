#!/usr/bin/env python3
"""Measure line and branch coverage and hold it to DOSSIER.md §21.4 (QA-001).

  python3.12 scripts/ci/coverage.py rust --out <rust.json> [--branch] [--export-only]
      Build and run the Rust workspace's tests with `-C instrument-coverage` in a separate
      target directory, merge the profiles and export a per-file summary with the `llvm-tools`
      matching the compiler's LLVM. `--export-only` reuses the last test phase in that directory.
  uv run --project python python scripts/ci/coverage.py python --out <python.json>
      Run the `python/` package's tests under the standard-library line tracer
      (`scripts/ci/pycoverage.py`).
  python3.12 scripts/ci/coverage.py check --rust <rust.json> --python <python.json>
          [--rust-branches <branch-run.json>] [--out <report.json>]
      Group the measurements by `scripts/ci/coverage-expectations.json` and fail when a group
      is under its floor or a required measurement is missing.

Lines are held to the floors from a line-only run, which `llvm-cov` exports in full. Branch
coverage needs rustc's unstable `-Z coverage-options=branch` (enabled on the pinned stable
compiler with `RUSTC_BOOTSTRAP=1`, for a measurement build only) and `llvm-cov` 22 crashes on
some files built that way, so a `--branch` run is reported beside the lines, per area, over the
files it could export; the exporter bisects the file set so a crash costs only that file's row.

Python 3.11+ ; standard library only (the `python` subcommand needs the project's pytest).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

ROOT = Path(__file__).resolve().parents[2]
EXPECTATIONS = ROOT / "scripts" / "ci" / "coverage-expectations.json"
DEFAULT_TARGET = ROOT / "target" / "coverage"


def load_expectations(path: Path = EXPECTATIONS) -> dict:
    return json.loads(path.read_text())


# --------------------------------------------------------------------------- rust


def _host_and_llvm(rustc: Sequence[str]) -> Tuple[str, str]:
    info = subprocess.run([*rustc, "-vV"], cwd=ROOT, capture_output=True, text=True, check=True).stdout
    fields = dict(line.split(": ", 1) for line in info.splitlines() if ": " in line)
    return fields["host"], fields.get("LLVM version", "")


def llvm_tool(name: str) -> Path:
    """The `llvm-tools` binary for the LLVM the building rustc uses (never a system LLVM).

    The active `rustc` may be a distribution build without the component (Homebrew's, on this
    project's development hosts); the rustup toolchain `rust-toolchain.toml` pins is tried next.
    Either way the tool must share the compiler's LLVM major version, or its profiles would be
    misread.
    """
    host, llvm = _host_and_llvm(["rustc"])
    candidates: List[Tuple[Sequence[str], str]] = [(["rustc"], "active rustc")]
    if shutil.which("rustup"):
        candidates.append((["rustup", "run", pinned_toolchain(), "rustc"], "rustup toolchain"))
    for rustc, label in candidates:
        try:
            sysroot = subprocess.run(
                [*rustc, "--print", "sysroot"], cwd=ROOT, capture_output=True, text=True, check=True
            ).stdout.strip()
        except (subprocess.CalledProcessError, FileNotFoundError):
            continue
        tool = Path(sysroot) / "lib" / "rustlib" / host / "bin" / name
        if not tool.is_file():
            continue
        version = subprocess.run([str(tool), "--version"], capture_output=True, text=True).stdout
        major = llvm.split(".")[0]
        if major and f"LLVM version {major}." not in version:
            raise SystemExit(f"coverage: {tool} ({label}) is not LLVM {llvm}, the compiler's")
        return tool
    raise SystemExit(f"coverage: no {name}; install it with `rustup component add llvm-tools`")


def pinned_toolchain() -> str:
    for line in (ROOT / "rust-toolchain.toml").read_text().splitlines():
        if line.strip().startswith("channel"):
            return line.split("=", 1)[1].strip().strip('"')
    return "stable"


def rust_sources(expectations: dict) -> List[str]:
    """Every Rust source file a person maintains, repository-relative."""
    rust = expectations["rust"]
    excluded = tuple(rust.get("exclude", []))
    files = []
    for path in sorted((ROOT / "crates").glob("*/src/**/*.rs")):
        relative = path.relative_to(ROOT).as_posix()
        if not relative.startswith(excluded):
            files.append(relative)
    return files


#: Continuous mode (`%c`): counters are mapped into the profile file as they change, so a
#: process the suites stop with SIGKILL (every served `quansio-server`) still leaves its profile;
#: without it, everything reached over HTTP would count as uncovered. One file per process
#: (`%p`): continuous mode cannot share pooled files between live processes.
PROFILE_PATTERN = "%c%p.profraw"


def continuous_mode_flags(platform: str = sys.platform) -> List[str]:
    """What continuous mode needs from the build on this platform."""
    if platform == "darwin":
        # The runtime maps the counter, data and bitmap sections page by page (16 KiB pages).
        return [
            f"-C link-arg=-Wl,-sectalign,__DATA,{section},0x4000"
            for section in ("__llvm_prf_cnts", "__llvm_prf_data", "__llvm_prf_bits")
        ]
    return ["-C llvm-args=-runtime-counter-relocation"]


def coverage_env(target: Path, branch: bool) -> Dict[str, str]:
    env = os.environ.copy()
    flags = ["-C instrument-coverage", *continuous_mode_flags()]
    if branch:
        env["RUSTC_BOOTSTRAP"] = "1"
        flags.append("-Z coverage-options=branch")
    env["RUSTFLAGS"] = " ".join(flags)
    env["CARGO_TARGET_DIR"] = str(target)
    env["LLVM_PROFILE_FILE"] = str(target / "profraw" / PROFILE_PATTERN)
    return env


def compile_env(env: Dict[str, str]) -> Dict[str, str]:
    """The environment for compiling: rustc writes no profile.

    rustc loads the instrumented proc macros while compiling. With continuous mode (`%c`) in
    LLVM_PROFILE_FILE their counters are mapped into a profile file that can be shorter than the
    mapping, and rustc dies with SIGBUS on the Linux runner.
    """
    return {**env, "LLVM_PROFILE_FILE": os.devnull}


def test_executables(env: Dict[str, str]) -> List[str]:
    """Every test binary and workspace binary `cargo test` builds (the objects to map)."""
    listing = subprocess.run(
        ["cargo", "test", "--workspace", "--no-run", "--message-format=json"],
        cwd=ROOT,
        env=compile_env(env),
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    objects = set()
    for line in listing.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if message["target"].get("kind") in (["custom-build"], ["proc-macro"]):
            continue
        objects.add(message["executable"])
    return sorted(objects)


def export(
    llvm_cov: Path, profdata: Path, objects: Sequence[str], sources: Sequence[str]
) -> Optional[List[dict]]:
    """Per-file summaries for `sources`, or None when `llvm-cov` crashes on them."""
    command = [str(llvm_cov), "export", "-summary-only", f"--instr-profile={profdata}"]
    command += [str(objects[0])] + [f"--object={obj}" for obj in objects[1:]]
    command += ["--sources", *[str(ROOT / source) for source in sources]]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        return None
    return json.loads(result.stdout)["data"][0]["files"]


def export_bisecting(
    llvm_cov: Path, profdata: Path, objects: Sequence[str], sources: Sequence[str]
) -> Tuple[List[dict], List[str]]:
    """Export every file `llvm-cov` can, isolating the ones it crashes on."""
    files = export(llvm_cov, profdata, objects, sources)
    if files is not None:
        return files, []
    if len(sources) == 1:
        return [], list(sources)
    middle = len(sources) // 2
    left, left_failed = export_bisecting(llvm_cov, profdata, objects, sources[:middle])
    right, right_failed = export_bisecting(llvm_cov, profdata, objects, sources[middle:])
    return left + right, left_failed + right_failed


def summarize_files(files: List[dict]) -> Dict[str, dict]:
    rows: Dict[str, dict] = {}
    for entry in files:
        relative = Path(entry["filename"]).resolve().relative_to(ROOT.resolve()).as_posix()
        summary = entry["summary"]
        rows[relative] = {
            "lines": summary["lines"]["count"],
            "lines_covered": summary["lines"]["covered"],
            "branches": summary["branches"]["count"],
            "branches_covered": summary["branches"]["covered"],
        }
    return rows


def measure_rust(out: Path, target: Path, branch: bool, keep: bool, export_only: bool = False) -> int:
    expectations = load_expectations()
    env = coverage_env(target, branch)
    profraw = target / "profraw"
    exit_file = target / "cargo-test-exit"
    if export_only:
        # Reuse a finished test phase: its profiles and the exit code it recorded.
        if not exit_file.is_file() or not profraw.is_dir():
            raise SystemExit(f"coverage: --export-only needs {profraw} and {exit_file} from a test phase")
        tests_exit = int(exit_file.read_text().strip())
    else:
        if profraw.exists():
            shutil.rmtree(profraw)
        profraw.mkdir(parents=True)
        subprocess.run(["cargo", "test", "--workspace", "--no-run"], cwd=ROOT, env=compile_env(env))
        tests_exit = subprocess.run(
            ["cargo", "test", "--workspace", "--no-fail-fast"], cwd=ROOT, env=env
        ).returncode
        exit_file.write_text(f"{tests_exit}\n")
    objects = test_executables(env)
    profdata = target / "merged.profdata"
    raw = sorted(str(path) for path in profraw.glob("*.profraw"))
    subprocess.run(
        [str(llvm_tool("llvm-profdata")), "merge", "-sparse", *raw, "-o", str(profdata)],
        check=True,
    )
    sources = rust_sources(expectations)
    files, unreportable = export_bisecting(llvm_tool("llvm-cov"), profdata, objects, sources)
    measured = summarize_files(files)
    # A file no test binary links (no instrumented code of it ran or was mapped) is still a
    # file a person maintains: it counts as measured with nothing covered only when it has
    # code, which `llvm-cov` cannot tell us here, so it is listed rather than guessed.
    unmapped = [source for source in sources if source not in measured and source not in unreportable]
    summary = {
        "tool": "rustc -C instrument-coverage (continuous profiles)"
        + (" -Z coverage-options=branch" if branch else ""),
        "cargo_test_exit": tests_exit,
        "objects": len(objects),
        "profiles": len(raw),
        "files": measured,
        "unreportable": unreportable,
        "unmapped": unmapped,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(summary, indent=2) + "\n")
    if not keep:
        shutil.rmtree(profraw, ignore_errors=True)
    print(
        f"rust coverage: {len(measured)} files measured, {len(unreportable)} unreportable, "
        f"{len(unmapped)} unmapped; cargo test exit {tests_exit}"
    )
    return tests_exit


# --------------------------------------------------------------------------- check


def percent(covered: int, total: int) -> Optional[float]:
    return round(100.0 * covered / total, 2) if total else None


def has_code(path: Path) -> bool:
    """Whether a Rust file holds anything that executes (a function or closure body).

    A `lib.rs`/`mod.rs` of declarations and re-exports compiles to no coverage region, so no
    test binary maps it; that is not a gap.
    """
    try:
        text = path.read_text()
    except OSError:
        return True
    code = "\n".join(line.split("//", 1)[0] for line in text.splitlines())
    return re.search(r"\bfn\s+\w+|\|[^|]*\|\s*\{", code) is not None


def group_of(path: str, areas: Dict[str, List[str]]) -> Optional[str]:
    """The safety-critical area a file belongs to (longest matching prefix wins)."""
    best: Optional[Tuple[int, str]] = None
    for area, prefixes in areas.items():
        for prefix in prefixes:
            if path.startswith(prefix) and (best is None or len(prefix) > best[0]):
                best = (len(prefix), area)
    return best[1] if best else None


def check(
    rust: dict,
    python: Optional[dict],
    expectations: dict,
    branches: Optional[dict] = None,
    typescript: Optional[dict] = None,
) -> dict:
    """Group the line run's files against the floors; take branch counts from the branch run.

    Lines come from a line-only instrumented run, which `llvm-cov` exports in full. Branch
    counts come from a separate run with branch regions (`--branch`), reported per area over the
    files `llvm-cov` could export from it; §21.4 asks for them reported, not held to a floor.
    """
    rust_rules = expectations["rust"]
    areas: Dict[str, List[str]] = rust_rules["safety_critical"]["areas"]
    groups: Dict[str, dict] = {
        area: {"lines": 0, "lines_covered": 0, "branches": 0, "branches_covered": 0, "files": 0}
        for area in areas
    }
    other = {"lines": 0, "lines_covered": 0, "branches": 0, "branches_covered": 0, "files": 0}
    for path, row in rust["files"].items():
        bucket = groups[area] if (area := group_of(path, areas)) else other
        for key in ("lines", "lines_covered"):
            bucket[key] += row[key]
        bucket["files"] += 1
    branch_source = branches if branches is not None else rust
    for path, row in branch_source["files"].items():
        bucket = groups[area] if (area := group_of(path, areas)) else other
        for key in ("branches", "branches_covered"):
            bucket[key] += row[key]

    findings: List[str] = []
    floor = rust_rules["safety_critical"]["line_min"]
    results: Dict[str, dict] = {}
    for area, bucket in groups.items():
        line_pct = percent(bucket["lines_covered"], bucket["lines"])
        branch_pct = percent(bucket["branches_covered"], bucket["branches"])
        results[area] = {**bucket, "line_percent": line_pct, "branch_percent": branch_pct, "line_min": floor}
        if bucket["files"] == 0:
            findings.append(f"safety-critical area {area}: no measured file under {areas[area]}")
        elif line_pct is not None and line_pct < floor:
            findings.append(f"safety-critical area {area}: {line_pct}% lines < {floor}%")
    other_floor = rust_rules["other"]["line_min"]
    other_pct = percent(other["lines_covered"], other["lines"])
    results["rust-other"] = {**other, "line_percent": other_pct, "line_min": other_floor}
    if other_pct is not None and other_pct < other_floor:
        findings.append(f"other Rust: {other_pct}% lines < {other_floor}%")
    if rust.get("cargo_test_exit") != 0:
        findings.append(f"the instrumented cargo test exited {rust.get('cargo_test_exit')}")
    for path in rust.get("unreportable", []):
        if group_of(path, areas):
            findings.append(f"{path}: llvm-cov could not export this safety-critical file")
    for path in rust.get("unmapped", []):
        if group_of(path, areas) and has_code(ROOT / path):
            findings.append(f"{path}: no test binary maps this safety-critical file")

    python_rules = expectations["python"]
    if python is None:
        findings.append("python: no measurement")
    else:
        results["python"] = {
            "lines": python["lines"],
            "lines_covered": python["covered"],
            "line_percent": python["percent"],
            "line_min": python_rules["line_min"],
        }
        if python.get("pytest_exit") != 0:
            findings.append(f"python: the measured pytest exited {python.get('pytest_exit')}")
        if python["percent"] < python_rules["line_min"]:
            findings.append(f"python: {python['percent']}% lines < {python_rules['line_min']}%")

    typescript_rules = expectations["typescript"]
    if typescript is None:
        results["typescript"] = {"line_min": typescript_rules["line_min"], "status": "UNMEASURED",
                                 "reason": "run `coverage.py typescript` and pass --typescript"}
    else:
        results["typescript"] = {"lines": typescript["lines"], "lines_covered": typescript["covered"],
                                 "line_percent": typescript["percent"], "line_min": typescript_rules["line_min"]}
        if typescript.get("vitest_exit") != 0:
            findings.append(f"typescript: the measured vitest run exited {typescript.get('vitest_exit')}")
        if typescript["percent"] < typescript_rules["line_min"]:
            findings.append(f"typescript: {typescript['percent']}% lines < {typescript_rules['line_min']}%")
    return {
        "source": expectations["source"],
        "status": "FAIL" if findings else "PASS",
        "findings": findings,
        "groups": results,
        "unreportable": rust.get("unreportable", []),
        "unmapped": rust.get("unmapped", []),
        "branch_run": {
            "tool": branch_source.get("tool"),
            "unreportable": branch_source.get("unreportable", []),
        },
    }


# --------------------------------------------------------------------------- typescript

TS_PACKAGES = ("apps/desktop", "apps/web", "sdk/typescript")


def measure_typescript(out: Path) -> int:
    """Measure TypeScript line coverage of every package's `src/` with vitest's v8 provider."""
    packages: Dict[str, dict] = {}
    worst = 0
    scratch = out.parent / "typescript-raw"
    for package in TS_PACKAGES:
        report_dir = scratch / package.replace("/", "-")
        shutil.rmtree(report_dir, ignore_errors=True)
        run = subprocess.run(
            ["npx", "vitest", "run", "--coverage", "--coverage.provider=v8", "--coverage.reporter=json-summary",
             "--coverage.include=src/**", "--coverage.exclude=**/*.d.ts", f"--coverage.reportsDirectory={report_dir}"],
            cwd=ROOT / package,
        )
        worst = worst or run.returncode
        summary = json.loads((report_dir / "coverage-summary.json").read_text())["total"]["lines"]
        packages[package] = {"lines": summary["total"], "lines_covered": summary["covered"]}
    lines = sum(row["lines"] for row in packages.values())
    covered = sum(row["lines_covered"] for row in packages.values())
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({"packages": packages, "lines": lines, "covered": covered,
                               "percent": percent(covered, lines), "vitest_exit": worst}, indent=2) + "\n")
    print(f"typescript: {percent(covered, lines)}% lines ({covered}/{lines})")
    return worst


# --------------------------------------------------------------------------- main


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Measure coverage against DOSSIER.md §21.4")
    commands = parser.add_subparsers(dest="command", required=True)
    rust = commands.add_parser("rust")
    rust.add_argument("--out", type=Path, required=True)
    rust.add_argument("--target-dir", type=Path, default=DEFAULT_TARGET)
    rust.add_argument(
        "--branch",
        action="store_true",
        help="add branch regions (unstable rustc option; llvm-cov 22 cannot export some such files)",
    )
    rust.add_argument("--keep-profiles", action="store_true")
    rust.add_argument("--export-only", action="store_true", help="reuse the last test phase in --target-dir")
    typescript = commands.add_parser("typescript")
    typescript.add_argument("--out", type=Path, required=True)
    python = commands.add_parser("python")
    python.add_argument("--out", type=Path, required=True)
    verdict = commands.add_parser("check")
    verdict.add_argument("--rust", type=Path, required=True)
    verdict.add_argument("--python", type=Path)
    verdict.add_argument("--typescript", type=Path)
    verdict.add_argument("--rust-branches", type=Path, help="a --branch run, for branch counts")
    verdict.add_argument("--out", type=Path)
    args = parser.parse_args(argv)

    if args.command == "rust":
        return measure_rust(
            args.out, args.target_dir, args.branch, args.keep_profiles, args.export_only
        )
    if args.command == "typescript":
        return measure_typescript(args.out)
    if args.command == "python":
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import pycoverage

        return pycoverage.main(["--out", str(args.out)])
    report = check(
        json.loads(args.rust.read_text()),
        json.loads(args.python.read_text()) if args.python else None,
        load_expectations(),
        json.loads(args.rust_branches.read_text()) if args.rust_branches else None,
        json.loads(args.typescript.read_text()) if args.typescript else None,
    )
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2) + "\n")
    for name, group in report["groups"].items():
        if "status" in group:
            print(f"{name:<20} {group['status']} (floor {group['line_min']}%): {group['reason']}")
            continue
        print(f"{name:<20} {group.get('line_percent')}% lines (floor {group['line_min']}%)"
              + (f", {group['branch_percent']}% branches" if group.get("branch_percent") is not None else ""))
    for finding in report["findings"]:
        print(f"FINDING {finding}")
    print(f"coverage: {report['status']}")
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
