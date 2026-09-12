#!/usr/bin/env python3
"""Supply-chain and dependency-security gate (OPS-007).

Single entry point that runs every supply-chain rule and exits non-zero on any
finding, printing `rule: detail`:

  lockfiles   every manifest dependency is pinned by Cargo.lock / pnpm-lock.yaml /
              python/uv.lock (`unpinned-rust-dependency`, `unpinned-node-dependency`,
              `unpinned-python-dependency`).
  policy      deny.toml is structurally complete and its licence allow-list covers
              the declared licences; cargo-deny is run when installed
              (`deny-config-incomplete`, `license-not-allowlisted`, `cargo-deny`).
  sbom        a deterministic CycloneDX SBOM is generated to
              `artifacts/ci/sbom-<commit>.json` and verified against itself
              (`sbom-lockfile-missing`, `sbom-nondeterministic`, `sbom-empty`,
              `sbom-drift`).
  advisories  resolved packages are scanned against the committed offline advisory
              corpus (`vulnerable-dependency`).
  quarantine  imported skill/tool manifests carry the full quarantine -> provenance
              -> review -> normalization -> evaluation -> approved lifecycle
              (`skill-lifecycle-incomplete`, `skill-self-promoted`,
              `skill-manifest-unparsable`).

Optional tools that need the network degrade to an informational result
(`cargo-deny-unavailable`); they never make the gate fail and never make it pass
silently. Standard library only; no network access is required.

  python3.12 scripts/ci/supply_chain/check.py
  python3.12 scripts/ci/supply_chain/check.py --only policy --json
  python3.12 scripts/ci/supply_chain/check.py --root <tree> --only lockfiles
  python3.12 scripts/ci/supply_chain/check.py --verify-sbom artifacts/ci/sbom-<commit>.json
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import List, Optional, Sequence, Tuple

ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:  # runnable as a script and importable as a module
    sys.path.insert(0, str(ROOT))

from scripts.ci.supply_chain import advisories, lockfiles, policy, sbom, skills  # noqa: E402
from scripts.ci.supply_chain.finding import Finding  # noqa: E402

DEFAULT_ADVISORY_DB = ROOT / "tests" / "ci" / "fixtures" / "supply_chain" / "advisory-db"
DEFAULT_ARTIFACTS = ROOT / "artifacts" / "ci"
MODES = ("lockfiles", "policy", "sbom", "advisories", "quarantine")


def run(
    root: Path = ROOT,
    advisory_db: Optional[Path] = None,
    artifacts_dir: Optional[Path] = None,
    only: Optional[Sequence[str]] = None,
    run_cargo_deny: bool = True,
    cargo_lock: Optional[Path] = None,
    commit: Optional[str] = None,
    sbom_recorded: Optional[Path] = None,
) -> Tuple[List[Finding], List[Finding]]:
    """Run the selected rules; returns (findings, informational results)."""
    selected = set(only) if only else set(MODES)
    unknown = selected - set(MODES)
    if unknown:
        raise ValueError(f"unknown modes: {sorted(unknown)}")
    findings: List[Finding] = []
    informational: List[Finding] = []

    if "lockfiles" in selected:
        findings.extend(lockfiles.check(root))
    if "policy" in selected:
        policy_findings, policy_info = policy.check(root, run_external=run_cargo_deny)
        findings.extend(policy_findings)
        informational.extend(policy_info)
    if "sbom" in selected:
        sbom_findings, _ = sbom.check(
            root, artifacts_dir or DEFAULT_ARTIFACTS, commit=commit
        )
        findings.extend(sbom_findings)
    if sbom_recorded is not None:
        findings.extend(sbom.verify(root, sbom_recorded, commit=commit))
    if "advisories" in selected:
        findings.extend(
            advisories.scan(
                root, advisory_db or DEFAULT_ADVISORY_DB, cargo_lock=cargo_lock
            )
        )
    if "quarantine" in selected:
        findings.extend(skills.scan(root))
    return findings, informational


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Supply-chain and dependency-security gate (OPS-007)")
    parser.add_argument("--root", default=str(ROOT), help="tree to gate (defaults to the repository root)")
    parser.add_argument(
        "--only",
        action="append",
        default=[],
        choices=MODES,
        help="run only this mode (repeatable; default: every mode)",
    )
    parser.add_argument("--advisories", default=None, help="offline advisory corpus directory")
    parser.add_argument("--artifacts-dir", default=None, help="directory for the generated SBOM")
    parser.add_argument("--cargo-lock", default=None, help="scan this Cargo.lock instead of the tree's")
    parser.add_argument("--commit", default=None, help="commit recorded in the SBOM metadata")
    parser.add_argument(
        "--verify-sbom",
        default=None,
        help="recorded SBOM to re-generate and compare against (fails on drift)",
    )
    parser.add_argument("--json", action="store_true")
    parser.add_argument(
        "--no-cargo-deny",
        action="store_true",
        help="skip the optional external cargo-deny run",
    )
    args = parser.parse_args(argv)

    findings, informational = run(
        root=Path(args.root),
        advisory_db=Path(args.advisories) if args.advisories else None,
        artifacts_dir=Path(args.artifacts_dir) if args.artifacts_dir else None,
        only=args.only,
        run_cargo_deny=not args.no_cargo_deny,
        cargo_lock=Path(args.cargo_lock) if args.cargo_lock else None,
        commit=args.commit,
        sbom_recorded=Path(args.verify_sbom) if args.verify_sbom else None,
    )
    if args.json:
        print(
            json.dumps(
                {
                    "findings": [finding.__dict__ for finding in findings],
                    "informational": [finding.__dict__ for finding in informational],
                    "status": "FAIL" if findings else "PASS",
                },
                indent=2,
            )
        )
    else:
        for finding in informational:
            print(finding)
        for finding in findings:
            print(finding)
        print(
            f"supply-chain: {'CLEAN' if not findings else str(len(findings)) + ' finding(s)'} "
            f"({len(informational)} informational)"
        )
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
