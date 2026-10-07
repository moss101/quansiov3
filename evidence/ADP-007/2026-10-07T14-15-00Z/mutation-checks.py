#!/usr/bin/env python3
"""ADP-007 mutation checks: break each guard on purpose and show a named test then fails.

A mutant is KILLED only when all three hold: it COMPILES (the build step passes), the unmutated suite passes first
(CONTROL-PASSED for its toolchain), and a *named test* then fails. A mutant that does not compile, a control that
fails, or a failure with no named failing test is NO-VERDICT, never KILLED. A mutant that survives is SURVIVED and is a
hole in the tests.

Usage: mutation-checks.py <repo> <log> [name-filter | --check-patterns]
Run from a clean, committed tree; every mutant is restored with `git checkout -- <file>`.
The Rust suites need the private test database (QUANSIO_TEST_POSTGRES_URL); the real-plane suites need `uv` and the
development stub provider (QUANSIO_TEST_STUB_PROVIDER_URL, default http://127.0.0.1:59020).
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
ENV.setdefault("CARGO_INCREMENTAL", "0")

SRV = "crates/server/src/"
RT = SRV + "control/routines/"
SCH = SRV + "scheduler/"
WF = SCH + "workflow/"
API = SRV + "api/"
DESK = "apps/desktop/src/automation/"

CARGO_FAIL = re.compile(r"^test (\S+) \.\.\. FAILED", re.M)
VITEST_FAIL = re.compile(r"^\s*[×✗]\s+(.+?)(?:\s+\d+ms)?$", re.M)


def cargo(*selectors: str) -> tuple[list[str], list[str], str, re.Pattern[str]]:
    return (
        ["cargo", "test", "-p", "quansio-server", *selectors],
        ["cargo", "test", "-p", "quansio-server", *selectors, "--no-run"],
        ".",
        CARGO_FAIL,
    )


TOOLCHAINS = {
    "lib-routines": cargo("--lib", "control::routines::"),
    "lib-sched": cargo("--lib", "scheduler::"),
    "trig": cargo("--test", "automation_triggers"),
    "wf": cargo("--test", "automation_workflow"),
    "served": cargo("--test", "automation_served"),
    "untrusted": cargo("--test", "automation_untrusted"),
    "vitest": (
        ["../../node_modules/.bin/vitest", "run", "tests/automation.test.ts", "tests/automation-triggers.test.ts",
         "tests/automation-parts.test.ts", "--reporter=verbose"],
        ["../../node_modules/.bin/tsc", "-p", "tsconfig.json"],
        "apps/desktop",
        VITEST_FAIL,
    ),
}

# (name, toolchain, file, old, new)  -- old/new may be lists when a guard has a second layer that must go too.
MUTANTS = [
    # ---- A. replay protection, authentication, bounds, filters
    ("replay: the purge of old records removes every record", "trig", RT + "hooks.rs",
     "let cutoff = now - delivery.retention;", "let cutoff = now + chrono::Duration::days(1);"),
    ("replay: a signature is not remembered", "trig", RT + "hooks.rs",
     "let digest = delivery.signature_digest.to_string();",
     "let digest = format!(\"{}-{}\", delivery.signature_digest, UlidGenerator::new().generate());"),
    ("replay: a seen delivery claims a fire", "trig", RT + "hooks.rs",
     "                    if inserted == 0 {\n                        // Seen before",
     "                    if false && inserted == 0 {\n                        // Seen before"),
    ("filters: a group holds when any condition holds", "trig", RT + "hooks.rs",
     "if group.iter().all(|condition| holds(condition, payload)) {",
     "if group.iter().any(|condition| holds(condition, payload)) {"),
    ("auth: a stale signature is accepted", "trig", SRV + "notify/webhooks/inbound.rs",
     "Err(SignatureError::Stale) => return Err(InboundRefusal::Stale),", "Err(SignatureError::Stale) => {}"),
    ("auth: a wrong signature is accepted", "trig", SRV + "notify/webhooks/inbound.rs",
     "        Err(SignatureError::Malformed | SignatureError::Mismatch) => {\n            return Err(InboundRefusal::BadSignature)\n        }",
     "        Err(SignatureError::Malformed | SignatureError::Mismatch) => {}"),
    ("auth: a paused hook accepts a delivery", "trig", SRV + "notify/webhooks/inbound.rs",
     "    if routine.status == \"paused\" {", "    if false && routine.status == \"paused\" {"),
    ("auth: a malformed delivery id is accepted", "trig", SRV + "notify/webhooks/inbound.rs",
     "        Some(id) if valid_delivery_id(id) => id.to_string(),", "        Some(id) => id.to_string(),"),
    # The cap has three layers (the declared length, the stream's own limit, and the authenticator); removing one leaves the
    # others holding, so the mutant removes all three.
    ("bounds: the body is not capped", "trig", (API + "hooks.rs", SRV + "notify/webhooks/inbound.rs"),
     (["if declared.is_some_and(|length| length > config.max_body_bytes) {", "axum::body::to_bytes(body, config.max_body_bytes + 1)"],
      "    if body.len() > config.max_body_bytes {"),
     (["if false && declared.is_some_and(|length| length > config.max_body_bytes) {", "axum::body::to_bytes(body, usize::MAX)"],
      "    if false && body.len() > config.max_body_bytes {")),
    ("bounds: an endpoint has no rate limit", "trig", API + "hooks.rs",
     "    if !state.hook_limits().allow(&endpoint) {", "    if false && !state.hook_limits().allow(&endpoint) {"),
    # ---- B. management
    ("revision: a stale expected_revision is applied", "trig", RT + "store.rs",
     "Some(expected) if expected != current.revision => Err(RoutineError::RevisionConflict {",
     "Some(expected) if false && expected != current.revision => Err(RoutineError::RevisionConflict {"),
    ("revision: an update does not advance the revision", "trig", RT + "store.rs",
     "hook_secret_handle_id = $11, revision = revision + 1", "hook_secret_handle_id = $11, revision = revision"),
    ("create: the same request id with another definition is replayed", "trig", RT + "store.rs",
     "Some((view, stored)) if stored.as_deref() == Some(digest) => Ok(Some(CreatedRoutine {",
     "Some((view, stored)) if true || stored.as_deref() == Some(digest) => Ok(Some(CreatedRoutine {")
    ,
    ("limits: the enabled-trigger limit is one too high", "trig", RT + "store.rs",
     "    if active >= limits.max_enabled_triggers {", "    if active > limits.max_enabled_triggers {"),
    ("limits: the minimum interval is not enforced", "trig", RT + "store.rs",
     "    if seconds < limits.min_interval_seconds {", "    if false && seconds < limits.min_interval_seconds {"),
    ("dry run: it has no rate limit", "trig", API + "routines.rs",
     "    if !state.dry_run_limits().allow(&scope.tenant_id) {", "    if false && !state.dry_run_limits().allow(&scope.tenant_id) {"),
    # ---- C. time zones
    ("zones: a repeated wall-clock time fires at its later occurrence", "lib-sched", SCH + "timezone.rs",
     "LocalResult::Ambiguous(first, second) => Some(first.min(second)),",
     "LocalResult::Ambiguous(first, second) => Some(first.max(second)),"),
    ("zones: a skipped wall-clock time never fires", "lib-sched", SCH + "timezone.rs",
     "LocalResult::None => self.gap_end(local),", "LocalResult::None => None,"),
    ("zones: an unknown name falls back to UTC", "trig", SCH + "timezone.rs",
     "Tz::from_str(tz).map(Self::Iana).map_err(|_| unknown())",
     "Ok(Tz::from_str(tz).map_or_else(|_| Self::Fixed(FixedOffset::east_opt(0).expect(\"utc\")), Self::Iana))"),
    # ---- D. workflow scripts
    ("script: a finished single step is called again after a restart", "wf", WF + "drive.rs",
     "        if let Some(finished) = self.journal.finished(id) {\n            let ok =",
     "        if let Some(finished) = self.journal.finished(id).filter(|_| false) {\n            let ok ="),
    ("script: a finished item is called again after a restart", "wf", WF + "drive.rs",
     "                if let Some(finished) = self.journal.finished(&key) {",
     "                if let Some(finished) = self.journal.finished(&key).filter(|_| false) {"),
    ("script: a call is journalled as started every time", "wf", WF + "drive.rs",
     "        if !self.journal.started(key) {", "        if true {"),
    ("script: the call budget is not enforced", "wf", WF + "drive.rs",
     "        if needed_total > max {", "        if false {"),
    ("script: a fan-out above the threshold needs no approval", "wf", WF + "drive.rs",
     "        if needed > config.approval_threshold_calls && needed > self.journal.approved_calls() {", "        if false {"),
    ("script: an invalid item result is accepted", "wf", WF + "drive.rs",
     "        if violations.is_empty() {\n            Ok(value)", "        if true {\n            Ok(value)"),
    ("script: items start without regard to the cap", "wf", WF + "drive.rs",
     "                    .saturating_sub(self.journal.calls_used()),", "                    .saturating_sub(0),"),
    ("script: a script with two reducers over one step is valid", "lib-routines", RT + "script.rs",
     "                    if !reduced.insert(of.as_str()) {", "                    if false && !reduced.insert(of.as_str()) {"),
    ("script: an array bigger than max_items is valid", "lib-routines", RT + "script.rs",
     "                    if bound > *max_items as u64 {", "                    if false && bound > *max_items as u64 {"),
    ("script: more guaranteed calls than the cap is valid", "lib-routines", RT + "script.rs",
     "        if static_calls > self.max_agent_calls {", "        if false && static_calls > self.max_agent_calls {"),
    ("script: a turn a killed process left active is not recovered", "served", API + "workflow.rs",
     "                if matches!(outcome, RecoveryOutcome::Continue { .. }) {\n                    if let Some(turn) = turns.last() {",
     "                if false && matches!(outcome, RecoveryOutcome::Continue { .. }) {\n                    if let Some(turn) = turns.last() {"),
    ("script: a budget pause is not an attention item", "wf", SRV + "notify/notifier.rs",
     "                \"attention\",\n                format!(\n                    \"Script", "                \"info\",\n                format!(\n                    \"Script"),
    # ---- payload text is data (DOMAIN.md 12 rule 9)
    ("untrusted: a payload block is a plain text block", "trig", RT + "script.rs",
     "                \"type\": \"untrusted_data\",\n                \"source\": source,",
     "                \"type\": \"text\",\n                \"source\": source,"),
    ("untrusted: an untrusted block loses its label", "untrusted", SRV + "intelligence/model_source.rs",
     "                    trust_hint: if part.untrusted {", "                    trust_hint: if false && part.untrusted {"),
    ("untrusted: an untrusted block is merged into the text before it", "untrusted", SRV + "intelligence/model_source.rs",
     "            Some(last) if !untrusted && !last.untrusted => {", "            Some(last) if !last.untrusted => {"),
    ("untrusted: the boundary markers in a payload are not neutralised", "lib-routines", RT + "script.rs",
     "    (\"<<UNTRUSTED_DATA\", \"<<UNTRUSTED DATA\"),", "    (\"<<UNTRUSTED_DATA\", \"<<UNTRUSTED_DATA\"),"),
    # ---- E. desktop
    ("desktop: an update does not name the revision it was made from", "vitest", DESK + "routines.ts",
     "    expected_revision: expectedRevision,", "    expected_revision: undefined,"),
    ("desktop: pause and resume do not name the revision", "vitest", DESK + "routines.ts",
     "  return { routine_id: routine.id, expected_revision: routine.revision };", "  return { routine_id: routine.id };"),
    ("desktop: a kept result still holds the one-time secret", "vitest", DESK + "secret-once.ts",
     "  return { ...rest, secret: typeof secret === \"string\" ? \"shown once\" : null };", "  return { ...rest, secret };"),
    ("desktop: any refusal is a revision conflict", "vitest", DESK + "failures.ts",
     "  if (failure.code !== \"CONFLICT_REVISION\") {", "  if (false) {"),
]


def as_list(value):
    """A mutant's `old`/`new` is one string, or a list of strings when a guard has a second layer that must go too."""
    return value if isinstance(value, list) else [value]


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=REPO, check=True, capture_output=True, text=True).stdout


def run(cmd: list[str], cwd: str) -> tuple[int, str]:
    # A mutant may loop; a run that does not end in 15 minutes is cut off and has no named failing test.
    try:
        result = subprocess.run(cmd, cwd=REPO / cwd, env=ENV, capture_output=True, text=True, timeout=900)
    except subprocess.TimeoutExpired as error:
        return 124, f"TIMEOUT {error}"
    return result.returncode, result.stdout + result.stderr


def main() -> None:
    LOG.parent.mkdir(parents=True, exist_ok=True)
    out = open(LOG, "a")

    def say(line: str) -> None:
        print(line, flush=True)
        out.write(line + "\n")
        out.flush()

    if CHECK_ONLY:
        bad = 0
        for name, _tool, file, old, _new in MUTANTS:
            fs = file if isinstance(file, tuple) else (file,)
            os_ = old if isinstance(file, tuple) else (old,)
            count = min((REPO / f).read_text().count(part) for f, o in zip(fs, os_) for part in as_list(o))
            if count != 1:
                bad += 1
                say(f"PATTERN {count} {name}")
        say(f"patterns checked: {len(MUTANTS)}, bad: {bad}")
        return
    if git("status", "--porcelain", "--untracked-files=no").strip():
        sys.exit("the tree must be clean and committed")
    say(f"# ADP-007 mutation checks at {git('rev-parse', '--short', 'HEAD').strip()}")
    controls: dict[str, bool] = {}
    killed = survived = nov = 0
    for name, tool, file, old, new in MUTANTS:
        if FILTER and not any(part in name for part in FILTER.split("|")):
            continue
        test_cmd, build_cmd, cwd, failing = TOOLCHAINS[tool]
        if tool not in controls:
            run(build_cmd, cwd)
            code, text = run(test_cmd, cwd)
            controls[tool] = code == 0
            say(f"CONTROL-{'PASSED' if code == 0 else 'FAILED'} {tool}")
            if code != 0:
                say(text[-3000:])
        files = file if isinstance(file, tuple) else (file,)
        olds = old if isinstance(file, tuple) else (old,)
        news = new if isinstance(file, tuple) else (new,)
        originals = {f: (REPO / f).read_text() for f in files}
        if any(originals[f].count(part) != 1 for f, o in zip(files, olds) for part in as_list(o)):
            say(f"NO-VERDICT (pattern not found exactly once) {name}")
            nov += 1
            continue
        try:
            for f, o, n in zip(files, olds, news):
                mutated = originals[f]
                for part, replacement in zip(as_list(o), as_list(n)):
                    mutated = mutated.replace(part, replacement, 1)
                (REPO / f).write_text(mutated)
            code, text = run(build_cmd, cwd)
            if code != 0:
                say(f"NO-VERDICT (does not compile) {name}")
                nov += 1
                continue
            say(f"COMPILES {name}")
            if not controls[tool]:
                say(f"NO-VERDICT (control failed) {name}")
                nov += 1
                continue
            code, text = run(test_cmd, cwd)
            names = sorted(set(failing.findall(text)))
            if code != 0 and names:
                killed += 1
                say(f"KILLED {name} <- {names[0][:110]}")
            elif code != 0:
                nov += 1
                say(f"NO-VERDICT (failed with no named test) {name}")
            else:
                survived += 1
                say(f"SURVIVED {name}")
        finally:
            subprocess.run(["git", "checkout", "--", *files], cwd=REPO, check=True)
    say(f"# mutants: {killed + survived + nov}  killed: {killed}  survived: {survived}  no-verdict: {nov}")


main()
