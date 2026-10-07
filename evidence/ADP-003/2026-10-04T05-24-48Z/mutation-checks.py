#!/usr/bin/env python3
"""ADP-003 mutation checks: break each guard on purpose and show a named test then fails.

A mutant is KILLED only when all three hold: it COMPILES (the build step passes), the unmutated suite
passes first (CONTROL-PASSED), and a *named test* then fails. A mutant that does not compile, a control
that fails, or a failure with no named failing test is NO-VERDICT, never KILLED (memory note
mutation-checks-must-compile). A mutant that survives is reported SURVIVED and is a hole in the tests.

Usage: mutation-checks.py <repo> <log> [name-filter | --check-patterns]
Run from a clean, committed tree; every mutant is restored with `git checkout -- <file>`.
Environment: QUANSIO_TEST_POSTGRES_URL (a private database), `uv`, macOS with Swift.
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
CHECK_ONLY = FILTER == "--check-patterns"
if CHECK_ONLY:
    FILTER = ""

ENV = dict(os.environ)
ENV["QUANSIO_TEST_MACOS_RECORDER"] = "1"
ENV.setdefault("CARGO_INCREMENTAL", "0")
if not CHECK_ONLY and not ENV.get("QUANSIO_TEST_POSTGRES_URL"):
    sys.exit("QUANSIO_TEST_POSTGRES_URL must name a private database")

REC = "crates/server/src/control/skills/recording/"
API = "crates/server/src/api/"
SWIFT = "native/macos/Sources/QuansioMacBridge/Recording/"
PY = "python/intelligence/skills/"
DESKTOP = "apps/desktop/src/"

CARGO_FAIL = re.compile(r"^test (\S+) \.\.\. FAILED", re.M)

# tool -> (test command, build command, cwd, failing-test regex)
TOOLCHAINS = {
    "rust-lib": (
        ["cargo", "test", "-p", "quansio-server", "--lib", "recording"],
        ["cargo", "test", "-p", "quansio-server", "--lib", "--no-run"],
        ".",
        CARGO_FAIL,
    ),
    "rust-int": (
        ["cargo", "test", "-p", "quansio-server", "--test", "skill_recording"],
        ["cargo", "test", "-p", "quansio-server", "--test", "skill_recording", "--no-run"],
        ".",
        CARGO_FAIL,
    ),
    "swift": (
        ["swift", "test", "--package-path", "native/macos", "--filter",
         "RecordingRedactionTests|RecorderSessionTests|RecorderControlTests|RecorderProcessTests|RecordingScreenshotTests|SecureInputTests"],
        ["swift", "build", "--build-tests", "--package-path", "native/macos"],
        ".",
        re.compile(r"Test Case '-\[(\S+ \S+)\]' failed"),
    ),
    "python": (
        ["uv", "run", "--project", "python", "pytest", "-q", "-p", "no:cacheprovider",
         "python/tests/intelligence/test_recording_compiler.py", "python/tests/intelligence/test_recording_redaction.py",
         "python/tests/intelligence/test_recording_rpc.py", "python/tests/intelligence/test_skill_resolver.py"],
        ["uv", "run", "--project", "python", "python", "-c",
         "import intelligence.skills.recording, intelligence.server.servicer, intelligence.skills.models"],
        ".",
        re.compile(r"^FAILED (\S+)", re.M),
    ),
    "vitest": (
        ["npx", "vitest", "run", "tests/recording.test.ts", "tests/recorder.test.ts", "tests/preload.test.ts", "--reporter=verbose"],
        None,
        "apps/desktop",
        re.compile(r"^\s*×\s+(.+?)(?:\s+\d+ms)?$", re.M),
    ),
}

# (name, tool, file, old, new)
MUTANTS = [
    # ---- redaction (Rust)
    ("redact(rust): card numbers are no longer removed", "rust-lib", REC + "redact.rs",
     "out = replace_all(&out, &CARD, Category::CardNumber, &mut counts);", "let _ = &CARD;"),
    ("redact(rust): a secure field's text is kept", "rust-lib", REC + "redact.rs",
     "return Some(FieldRule::Secure);", "return None;"),
    # ---- archive validation (Rust)
    ("archive: another owner's archive is accepted (epoch)", "rust-lib", REC + "archive.rs",
     "|| owner.get(\"epoch\").and_then(Value::as_i64) != Some(expected.epoch)\n    {", "|| false\n    {"),
    ("archive: a recording the model 'finished' is accepted", "rust-lib", REC + "archive.rs",
     'if string(finish, "by", "finish")? != "user" {', "if false {"),
    ("archive: an archive that still holds a secret is stored", "rust-lib", REC + "archive.rs",
     "if !findings.pointers.is_empty() || !findings.categories.is_empty() {", "if false {"),
    ("archive: identical consecutive screenshots are accepted", "rust-lib", REC + "archive.rs",
     "if *last_sha == sha {", "if false {"),
    # ---- proposal validation (Rust)
    ("proposal: a coordinate key in a slot control is accepted", "rust-lib", REC + "proposal.rs",
     "if FORBIDDEN_KEYS.contains(&key.to_ascii_lowercase().as_str()) {", "if false {"),
    ("proposal: the plane can classify a slot for the person", "rust-lib", REC + "proposal.rs",
     'if !map.get("classification").is_none_or(Value::is_null) {', "if false {"),
    ("proposal: a capability no needed tool has is accepted", "rust-lib", REC + "proposal.rs",
     "if !allowed_classes.contains(&class) {", "if false {"),
    ("proposal: a redacted value can become a constant", "rust-lib", REC + "proposal.rs",
     'if value.is_empty() || value.contains("[REDACTED:") || field_rule(&attrs).is_some() {', "if false {"),
    ("proposal: a submission with an undecided slot is accepted", "rust-lib", REC + "proposal.rs",
     "if let Some(missing) = by_id.keys().find(|id| !decided.contains_key(*id)) {",
     "if let Some(missing) = by_id.keys().find(|_| false) {"),
    ("compile: the plane's answer is taken without the server's validation", "rust-lib", REC + "compile.rs",
     "proposal::validate(&proposal, recording_id, tools, config)?;", "let _ = (recording_id, tools, config);"),
    # ---- owner fence (Rust, unit)
    ("fence: another user is accepted", "rust-lib", REC + "store.rs",
     "if row.owner_user_id != caller.user_id || row.tenant_id != caller.tenant_id {", "if false {"),
    ("fence: another epoch is accepted", "rust-lib", REC + "store.rs",
     "if row.epoch != caller.epoch {", "if false {"),
    ("fence: another sign-in session is accepted", "rust-lib", REC + "store.rs",
     "if caller.session_family_id.as_ref() != Some(family) {", "if false {"),
    # ---- the store and the API against PostgreSQL and the real plane
    ("store: a stale epoch no longer aborts the recording", "rust-int", REC + "store.rs",
     "if same_user && row.status == RecordingStatus::Recording {", "if false {"),
    ("store: a stranger's call aborts the owner's recording", "rust-int", REC + "store.rs",
     "if same_user && row.status == RecordingStatus::Recording {", "if row.status == RecordingStatus::Recording {"),
    ("store: compile before the finish is not its own refusal", "rust-int", REC + "store.rs",
     "if row.status == RecordingStatus::Recording {\n            return Err(RecordingError::NotFinished { id: row.id });",
     "if false {\n            return Err(RecordingError::NotFinished { id: row.id });"),
    ("store: an archive that still holds a secret is stored (integration)", "rust-int", REC + "archive.rs",
     "if !findings.pointers.is_empty() || !findings.categories.is_empty() {", "if false {"),
    ("store: an owner change from another user is not refused (integration)", "rust-int", REC + "store.rs",
     "if row.owner_user_id != caller.user_id || row.tenant_id != caller.tenant_id {", "if false {"),
    # ---- the macOS recorder
    ("recorder: card numbers are no longer removed", "swift", SWIFT + "RecordingRedaction.swift",
     "out = replaceAll(out, card, .cardNumber, &counts)", "_ = card"),
    ("recorder: typed text is stored unredacted", "swift", SWIFT + "RecorderSession.swift",
     "text = result.text.map { String($0.prefix(limits.actionTextMaxChars)) }",
     "text = String(raw.prefix(limits.actionTextMaxChars))"),
    ("recorder: secure input does not pause capture", "swift", SWIFT + "RecorderSession.swift",
     'if secureInput.isSecureInputActive() {\n            setPause("secure_input", at: at)',
     'if false {\n            setPause("secure_input", at: at)'),
    ("recorder: text typed into a secure field is kept", "swift", SWIFT + "RecorderSession.swift",
     "if case .secure? = rule {", "if false {"),
    ("recorder: another account is accepted", "swift", SWIFT + "RecorderSession.swift",
     "if caller != owner {", "if false {"),
    ("recorder: a recording that ran out of time is not discarded", "swift", SWIFT + "RecorderSession.swift",
     "if clock.now().timeIntervalSince(startedAt) * 1000 > Double(limits.maxDurationMs) {", "if false {"),
    ("recorder: reaching the action limit finishes the recording", "swift", SWIFT + "RecorderSession.swift",
     "dropped += 1\n            return .droppedForLimit", "dropped += 1\n            state = .finished\n            return .droppedForLimit"),
    ("channel: the one-time token can be used twice", "swift", SWIFT + "RecorderControlServer.swift",
     "token = nil\n        channel = connection", "channel = connection"),
    ("channel: a bootstrap file others can read is accepted", "swift", SWIFT + "RecorderControlServer.swift",
     "guard (info.st_mode & 0o777) == 0o600 else {", "guard true else {"),
    ("channel: a socket directory others can enter is accepted", "swift", SWIFT + "RecorderControlServer.swift",
     "guard (info.st_mode & 0o077) == 0 else {", "guard true else {"),
    ("controller: a reused request id with other arguments is replayed", "swift", SWIFT + "RecorderController.swift",
     "if seen.fingerprint != fingerprint {", "if false {"),
    ("controller: capture starts without the permissions it needs", "swift", SWIFT + "RecorderController.swift",
     "if let missing = needed.first(where: { $0.1 != .granted }) {", "if false, let missing = needed.first(where: { $0.1 != .granted }) {"),
    ("screenshots: an identical screenshot is kept twice", "swift", SWIFT + "Screenshots.swift",
     "if sha == lastSha { return .duplicate }", ""),
    ("screenshots: a screenshot showing a secret is kept", "swift", SWIFT + "Screenshots.swift",
     "if !redacted.counts.isEmpty {", "if false {"),
    ("screenshots: the minimum interval is not kept", "swift", SWIFT + "Screenshots.swift",
     "if let last = lastAttempt, now.timeIntervalSince(last) * 1000 < Double(limits.screenshotMinIntervalMs) {",
     "if false, let last = lastAttempt, now.timeIntervalSince(last) * 1000 < Double(limits.screenshotMinIntervalMs) {"),
    # ---- the plane
    ("plane: card numbers are no longer removed", "python", PY + "recording/redact.py",
     "out = _replace(out, _CARD, CARD_NUMBER, counts)", "out = out"),
    ("plane: a projection that still holds a secret is compiled", "python", PY + "recording/compiler.py",
     "    if categories:\n        raise RecordingCompileError(", "    if False:\n        raise RecordingCompileError("),
    ("plane: the compiler classifies the person's slots", "python", PY + "recording/compiler.py",
     '"classification": None,', '"classification": "input",'),
    ("plane: a flagged control title is copied into the skill", "python", PY + "recording/compiler.py",
     "            if key in action.target and not self._flagged(action, key):\n                ui[key] = action.target[key][:200]",
     "            if key in action.target:\n                ui[key] = action.target[key][:200]"),
    ("plane: text that reads as an instruction is not flagged", "python", PY + "recording/compiler.py",
     "patterns = detect_injection(text)", "patterns = ()"),
    ("plane: a Tool is never preferred to pixels", "python", PY + "recording/compiler.py",
     "            if preferred:\n                index = self._tool_step(index, preferred)",
     "            if False:\n                index = self._tool_step(index, preferred)"),
    ("plane: the validator accepts a coordinate key", "python", PY + "recording/proposal.py",
     "            if str(key).lower() in FORBIDDEN_KEYS:", "            if False:"),
    ("plane: a skill manifest may carry a coordinate in its procedure", "python", PY + "models.py",
     "    if found:\n        raise SkillError(\n            \"VALIDATION_SCHEMA\",\n            RULE_PROCEDURE,",
     "    if False:\n        raise SkillError(\n            \"VALIDATION_SCHEMA\",\n            RULE_PROCEDURE,"),
    # ---- the desktop
    ("desktop: a finish request is a click whatever produced it", "vitest", DESKTOP + "recording/state.ts",
     "return { gesture: event.isTrusted };", "return { gesture: true };"),
    ("desktop: a submission with an undecided slot is built", "vitest", DESKTOP + "recording/state.ts",
     "if (open.length > 0) {", "if (false) {"),
    ("desktop: the generic command path sends a recording command", "vitest", DESKTOP + "main/ipc.ts",
     "if (RECORDING_COMMANDS.includes(command.name)) {", "if (false) {"),
    ("desktop: main finishes without the person's click", "vitest", DESKTOP + "main/recorder.ts",
     "if (!request.gesture) {", "if (false) {"),
    ("desktop: an account change leaves the recorder running", "vitest", DESKTOP + "main/recorder.ts",
     'if (this.#running) {\n      this.#teardown("account_changed");', 'if (false) {\n      this.#teardown("account_changed");'),
    ("desktop: compile and submit carry an epoch the page sent", "vitest", DESKTOP + "main/ipc.ts",
     "epoch: this.recording.epoch }", 'epoch: record["epoch"] }'),
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


if CHECK_ONLY:
    bad = 0
    for name, _tool, file, old, _new in MUTANTS:
        count = (REPO / file).read_text().count(old)
        if count != 1:
            print(f"PATTERN {count}x  {name}  ({file})")
            bad += 1
    print(f"{len(MUTANTS)} mutants, {bad} with a pattern that is not exactly once")
    sys.exit(1 if bad else 0)

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
