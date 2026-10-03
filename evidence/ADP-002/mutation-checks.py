#!/usr/bin/env python3
"""ADP-002 mutation checks: break each guard on purpose and show a named test then fails.

A mutant is KILLED only when all three hold: it COMPILES (the build step passes), the unmutated suite
passes first (CONTROL-PASSED), and a *named test* then fails. A mutant that does not compile, a control
that fails, or a failure with no named failing test is NO-VERDICT, never KILLED (memory note
mutation-checks-must-compile). A mutant that survives is reported SURVIVED and is a hole in the tests.

Usage: mutation-checks.py <repo> <log> [name-filter]
Run from a clean, committed tree; every mutant is restored with `git checkout -- <file>`.
Environment: QUANSIO_TEST_POSTGRES_URL (a private database), macOS with codesign.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(sys.argv[1]).resolve()
LOG = Path(sys.argv[2]).resolve()
FILTER = sys.argv[3] if len(sys.argv) > 3 else ""

ENV = dict(os.environ)
ENV["QUANSIO_TEST_MACOS_COMPUTER_USE"] = "1"
if not ENV.get("QUANSIO_TEST_POSTGRES_URL"):
    sys.exit("QUANSIO_TEST_POSTGRES_URL must name a private database")

MACHINE = "crates/machine/src/computer_use/"
TOOLS = "crates/tools/src/receipt.rs"
SWIFT = "native/macos/Sources/QuansioMacBridge/"
DESKTOP = "apps/desktop/src/"

# tool -> (test command, build command, cwd, failing-test regex)
TOOLCHAINS = {
    "machine": (
        ["cargo", "test", "-p", "quansio-machine", "--test", "computer_receipts", "--test", "computer_sequence",
         "--test", "computer_permission", "--test", "computer_helper"],
        ["cargo", "test", "-p", "quansio-machine", "--no-run", "--test", "computer_receipts", "--test",
         "computer_sequence", "--test", "computer_permission", "--test", "computer_helper"],
        ".",
        re.compile(r"^test (\S+) \.\.\. FAILED", re.M),
    ),
    "tools": (
        ["cargo", "test", "-p", "quansio-tools", "--test", "computer_receipt"],
        ["cargo", "test", "-p", "quansio-tools", "--no-run", "--test", "computer_receipt"],
        ".",
        re.compile(r"^test (\S+) \.\.\. FAILED", re.M),
    ),
    "swift": (
        ["swift", "test", "--package-path", "native/macos", "--filter",
         "ReceiptTests|BackgroundInvariantTests|PermissionStatusTests"],
        ["swift", "build", "--build-tests", "--package-path", "native/macos"],
        ".",
        re.compile(r"Test Case '-\[(\S+ \S+)\]' failed"),
    ),
    "vitest": (
        ["npx", "vitest", "run", "tests/permissions.test.ts", "--reporter=verbose"],
        None,
        "apps/desktop",
        re.compile(r"^\s*×\s+(.+?)(?:\s+\d+ms)?$", re.M),
    ),
}

# (name, tool, file, old, new)
MUTANTS = [
    # --- receipts: unknown is never rounded
    ("receipt: a measured violation no longer overrides the bridge's claim", "machine", MACHINE + "receipt.rs",
     "if self.invariant_violated || measured == Some(true) {", "if self.invariant_violated {"),
    ("receipt: an unmeasured background 'applied' is no longer lowered to unknown", "machine", MACHINE + "receipt.rs",
     "} else if measured.is_none() && self.effect_verified == Some(true) {", "} else if false {"),
    ("receipt: an unknown effect is written to the wire as verified", "machine", MACHINE + "receipt.rs",
     '"effect_verified": self.effect_verified,', '"effect_verified": self.effect_verified.or(Some(true)),'),
    ("receipt: 'verified' is accepted for an action that dispatched nothing", "machine", MACHINE + "receipt.rs",
     "if self.outcome != ReceiptOutcome::Refused || self.effect_verified != Some(false) {",
     "if self.outcome != ReceiptOutcome::Refused {"),
    ("receipt: an unverified action settles as success in the ledger", "machine", MACHINE + "receipt.rs",
     "ReceiptOutcome::Applied | ReceiptOutcome::Observed => LedgerOutcome::SettledSuccess,",
     "ReceiptOutcome::Applied | ReceiptOutcome::Observed | ReceiptOutcome::Unverified => LedgerOutcome::SettledSuccess,"),
    # --- dispatcher
    ("dispatcher: the bridge's receipt is used without being re-derived", "machine", MACHINE + "bridge.rs",
     ".normalized()",
     ".clone().check_coherence().map(|()| ActionReceipt::unverified(ReceiptMethod::HidEvent, ExecutionMethod::Foreground))"),
    ("dispatcher: a missing permission no longer stops the action", "machine", MACHINE + "bridge.rs",
     ".map_err(ComputerDispatchError::PermissionRequired)?;",
     ".map_err(ComputerDispatchError::PermissionRequired).or_else(|_| Ok::<(), ComputerDispatchError>(()))?;"),
    ("dispatcher: an input that failed after dispatch releases its slot (blind retry)", "machine", MACHINE + "bridge.rs",
     "Err(code) if action.is_consequential() =>", "Err(code) if false =>"),
    # --- sequence: terminal conditions
    ("sequence: a user stop is not checked", "machine", MACHINE + "sequence.rs",
     "if self.stop.is_stopped() {", "if false {"),
    ("sequence: a changed generation is not a takeover", "machine", MACHINE + "sequence.rs",
     "|| control.generation != generation", "|| false"),
    ("sequence: a pending takeover is not seen before the next action", "machine", MACHINE + "sequence.rs",
     "|| control.takeover_pending\n", "|| false\n"),
    ("sequence: the action limit is not enforced", "machine", MACHINE + "sequence.rs",
     "if done >= self.limits.max_actions {", "if false {"),
    ("sequence: the wall-time limit is not enforced", "machine", MACHINE + "sequence.rs",
     "} else if started.elapsed() >= self.limits.max_duration {", "} else if false {"),
    ("sequence: the typed-byte limit is not enforced", "machine", MACHINE + "sequence.rs",
     "} else if typed.saturating_add(next.typed_bytes()) > self.limits.max_typed_bytes {", "} else if false {"),
    ("sequence: a permission revoked mid-sequence parks instead of ending it", "machine", MACHINE + "sequence.rs",
     "&& !settled.is_empty();", "&& false;"),
    ("sequence: a receipt that says 'not applied' no longer halts the sequence", "machine", MACHINE + "sequence.rs",
     "ReceiptOutcome::NotApplied | ReceiptOutcome::InvariantViolated\n", "ReceiptOutcome::InvariantViolated\n"),
    # --- permission gate
    ("permission: a lapsed foreground grant is ignored", "machine", MACHINE + "permission.rs",
     "if drives_foreground(action) && foreground == ForegroundControl::Lapsed {", "if false {"),
    ("permission: a restricted permission sends the person to Settings", "machine", MACHINE + "permission.rs",
     "guide: GuideAction::ContactAdministrator,",
     "guide: GuideAction::OpenSettings { pane: panes.pane(permission).to_string() },"),
    ("permission: a screenshot does not need Screen Recording", "machine", MACHINE + "permission.rs",
     "if *include_screenshot {", "if false {"),
    ("permission: an unknown permission in a status is ignored", "machine", MACHINE + "permission.rs",
     "if let Some(extra) = object.keys().find(|key| Permission::parse(key).is_none()) {",
     "if let Some(extra) = object.keys().find(|_| false) {"),
    # --- helper pin
    ("helper: the pinned bundle ids are not checked", "machine", MACHINE + "helper.rs",
     ".filter(|id| self.allowed_bundle_ids.iter().any(|allowed| allowed == id))", ".filter(|_| true)"),
    ("helper: the signature is not verified", "machine", MACHINE + "helper.rs",
     "if !verification.success {", "if false {"),
    ("helper: the designated requirement is not tested", "machine", MACHINE + "helper.rs",
     "if !test.success {", "if false {"),
    ("helper: the hardened runtime is not required", "machine", MACHINE + "helper.rs",
     "if !identity.hardened_runtime {", "if false {"),
    ("helper: an installed version is not held to its fingerprint", "machine", MACHINE + "helper.rs",
     "if verified.fingerprint != id {", "if false {"),
    ("helper: an old version is never pruned", "machine", MACHINE + "helper.rs",
     "fs::remove_dir_all(path)?;", "let _ = path;"),
    ("helper: a rollback forgets the version it replaced", "machine", MACHINE + "helper.rs",
     'self.write_pointer("previous", active.as_deref())?;', 'self.write_pointer("previous", None)?;'),
    ("helper: a failed probe does not restore the active pointer", "machine", MACHINE + "helper.rs",
     'self.write_pointer("active", before_active.as_deref())?;', "let _ = &before_active;"),
    ("helper: the orphan sweep ignores where the helper runs from", "machine", MACHINE + "helper.rs",
     "if !command.starts_with(&prefix) || !command.contains(&profile) {", "if !command.contains(&profile) {"),
    ("helper: the orphan sweep kills the runtime's own helper", "machine", MACHINE + "helper.rs",
     "if pid == own || keep.contains(&pid) {", "if pid == own {"),
    # --- configuration
    ("config: the Apple anchor can be switched off", "machine", MACHINE + "config.rs",
     'for key in ["require_apple_anchor", "require_hardened_runtime"] {', 'for key in ["require_hardened_runtime"] {'),
    ("config: a published helper need not name a team", "machine", MACHINE + "config.rs",
     "if status == HelperStatus::Published && team_id.is_none() {", "if false {"),
    # --- Tool Registry
    ("tools: a loosened receipt schema is accepted", "tools", TOOLS,
     "if receipt != &receipt_schema() {", "if false {"),
    ("tools: a verified effect is accepted for an action that dispatched nothing", "tools", TOOLS,
     'return if outcome == "refused" && verified == Some(false) && !violated {', 'return if outcome == "refused" {'),
    ("tools: a background action is verified without a measurement", "tools", TOOLS,
     "if !measured {", "if false {"),
    # --- native bridge
    ("native: a measured violation no longer overrides the claim", "swift", SWIFT + "Receipt.swift",
     "if receipt.invariantViolated || receipt.invariants?.violated == true {", "if receipt.invariantViolated {"),
    ("native: an unknown verdict is omitted instead of written as null", "swift", SWIFT + "Receipt.swift",
     "try container.encode(effectVerified, forKey: .effectVerified)",
     "try container.encodeIfPresent(effectVerified, forKey: .effectVerified)"),
    ("native: input during the window is never detected", "swift", SWIFT + "BackgroundInvariants.swift",
     "idleAfter < idleBefore + elapsed - epsilon", "false"),
    ("native: a moved cursor is never detected", "swift", SWIFT + "BackgroundInvariants.swift",
     "movedSystemCursor: cursorBefore != cursorAfter,", "movedSystemCursor: false,"),
    ("native: text that was not found is reported unknown instead of absent", "swift", SWIFT + "BackgroundInvariants.swift",
     "guard valueAfter.contains(typed) else { return false }", "guard valueAfter.contains(typed) else { return nil }"),
    ("native: a person's input never lapses the foreground grant", "swift", SWIFT + "PermissionStatus.swift",
     "return personInputAfterAgent && idle < pauseSeconds ? .lapsed : .held", "return .held"),
    ("native: an unknown Input Monitoring answer is read as granted", "swift", SWIFT + "PermissionStatus.swift",
     "default: return .notDetermined\n    }\n}\n\n/// `AVAuthorizationStatus`", "default: return .granted\n    }\n}\n\n/// `AVAuthorizationStatus`"),
    # --- desktop
    ("desktop: openSettings accepts extra fields (a destination)", "vitest", DESKTOP + "main/permissions.ts",
     'if (Object.keys(record).some((key) => key !== "permission")) {', "if (false) {"),
    ("desktop: main opens whatever string the page names", "vitest", DESKTOP + "main/ipc.ts",
     "await open(settingsUrl(permission));", "await open(String(permission));"),
    ("desktop: a permission the run needs does not hold Continue back", "vitest", DESKTOP + "main/permissions.ts",
     'const permissionsReady = rows.every((row) => !row.needed || row.state === "granted");',
     "const permissionsReady = true;"),
    ("desktop: Continue is offered while a Settings step is outstanding", "vitest", DESKTOP + "live/permissions.ts",
     'return { kind: "open_settings", rows, canContinue: false, first: settings.permission };',
     'return { kind: "open_settings", rows, canContinue: true, first: settings.permission };'),
    ("desktop: an attention item may name another resume command", "vitest", DESKTOP + "live/permissions.ts",
     'if (source["resume_command"] !== "ResumeRun") {', "if (false) {"),
    ("desktop: input counts as paused immediately", "vitest", DESKTOP + "main/permissions.ts",
     "input.idleSeconds >= PAUSE_IDLE_SECONDS", "input.idleSeconds >= 0"),
    ("desktop: a permission with no probe sends the person to Settings", "vitest", DESKTOP + "main/permissions.ts",
     'action: probed ? guideAction(state) : "none",', "action: guideAction(state),"),
    ("desktop: an unknown media status is read as granted", "vitest", DESKTOP + "main/permissions.ts",
     'default:\n      return "not_determined";', 'default:\n      return "granted";'),
]


def run(cmd, cwd):
    return subprocess.run(cmd, cwd=REPO / cwd, env=ENV, capture_output=True, text=True)


def out_of(process):
    return process.stdout + "\n" + process.stderr


controls: dict[str, bool] = {}
lines: list[str] = []


def log(line: str) -> None:
    print(line, flush=True)
    lines.append(line)
    LOG.write_text("\n".join(lines) + "\n")


status = subprocess.run(["git", "status", "--porcelain"], cwd=REPO, capture_output=True, text=True).stdout.strip()
if status:
    sys.exit("the tree must be clean and committed before mutating:\n" + status)

killed = survived = no_verdict = 0
for name, tool, file, old, new in MUTANTS:
    if FILTER and FILTER not in name:
        continue
    test_cmd, build_cmd, cwd, failing = TOOLCHAINS[tool]
    path = REPO / file
    source = path.read_text()
    if source.count(old) != 1:
        log(f"NO-VERDICT  {name}: pattern found {source.count(old)} times in {file}")
        no_verdict += 1
        continue
    if tool not in controls:
        control = run(test_cmd, cwd)
        controls[tool] = control.returncode == 0
        log(f"CONTROL-{'PASSED' if controls[tool] else 'FAILED'}  {tool}")
        if not controls[tool]:
            log(out_of(control)[-2000:])
    if not controls[tool]:
        log(f"NO-VERDICT  {name}: the unmutated {tool} suite does not pass")
        no_verdict += 1
        continue
    try:
        path.write_text(source.replace(old, new, 1))
        if build_cmd is not None:
            build = run(build_cmd, cwd)
            if build.returncode != 0:
                log(f"NO-VERDICT  {name}: the mutant does not compile")
                no_verdict += 1
                continue
            log(f"COMPILES    {name}")
        result = run(test_cmd, cwd)
        failed = sorted(set(failing.findall(out_of(result))))
        failed = [f if isinstance(f, str) else " ".join(f) for f in failed]
        if result.returncode != 0 and failed:
            log(f"KILLED      {name} -- {failed[0]}" + (f" (+{len(failed) - 1} more)" if len(failed) > 1 else ""))
            # What the first failing test said, so a kill by the intended assertion is distinguishable from a
            # kill by an unrelated failure (a timeout, a connection).
            said = re.search(r"panicked at [^\n]+\n([^\n]+)", out_of(result))
            if said:
                log(f"            says: {said.group(1)[:200]}")
            killed += 1
        elif result.returncode != 0:
            log(f"NO-VERDICT  {name}: the suite failed but no named test did")
            no_verdict += 1
        else:
            log(f"SURVIVED    {name}")
            survived += 1
    finally:
        subprocess.run(["git", "checkout", "--", file], cwd=REPO, check=True)

log(f"SUMMARY killed={killed} survived={survived} no_verdict={no_verdict} total={killed + survived + no_verdict}")
sys.exit(0 if survived == 0 and no_verdict == 0 else 1)
