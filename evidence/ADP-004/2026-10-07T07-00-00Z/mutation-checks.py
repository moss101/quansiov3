#!/usr/bin/env python3
"""ADP-004 mutation checks: break each guard on purpose and show a named test then fails.

A mutant is KILLED only when all three hold: it COMPILES (the build step passes), the unmutated suite
passes first (CONTROL-PASSED), and a *named test* then fails. A mutant that does not compile, a control
that fails, or a failure with no named failing test is NO-VERDICT, never KILLED (memory note
mutation-checks-must-compile). A mutant that survives is reported SURVIVED and is a hole in the tests.

Usage: mutation-checks.py <repo> <log> [name-filter | --check-patterns]
Run from a clean, committed tree; every mutant is restored with `git checkout -- <file>`.
Environment: QUANSIO_TEST_POSTGRES_URL (a private database) for the Rust mutants.
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
if not CHECK_ONLY and not ENV.get("QUANSIO_TEST_POSTGRES_URL"):
    sys.exit("QUANSIO_TEST_POSTGRES_URL must name a private database")

DESKTOP = "apps/desktop/src/"
MAIN = DESKTOP + "main/"
SERVER = "crates/server/src/"

CARGO_FAIL = re.compile(r"^test (\S+) \.\.\. FAILED", re.M)

VITEST_FILES = [
    "tests/shell-config.test.ts",
    "tests/shortcuts.test.ts",
    "tests/shell-gates.test.ts",
    "tests/shell-windows.test.ts",
    "tests/shell-activity.test.ts",
    "tests/shell-menu.test.ts",
    "tests/shell-host.test.ts",
    "tests/shell-ui.test.ts",
    "tests/ipc-trust.test.ts",
    "tests/ipc-sources.test.ts",
    "tests/shell-modules.test.ts",
    "tests/security.test.ts",
    "tests/preload.test.ts",
]

# tool -> (test command, build command, cwd, failing-test regex)
TOOLCHAINS = {
    "rust-int": (
        ["cargo", "test", "-p", "quansio-server", "--test", "maintenance"],
        ["cargo", "test", "-p", "quansio-server", "--test", "maintenance", "--no-run"],
        ".",
        CARGO_FAIL,
    ),
    "vitest": (
        ["../../node_modules/.bin/vitest", "run", *VITEST_FILES, "--reporter=verbose"],
        ["../../node_modules/.bin/tsc", "-p", "tsconfig.json"],
        "apps/desktop",
        re.compile(r"^\s*[×✗]\s+(.+?)(?:\s+\d+ms)?$", re.M),
    ),
}

# (name, tool, file, old, new)
MUTANTS = [
    # ---- the maintenance status (server)
    ("status: a run that is waiting counts as a Turn in flight", "rust-int", SERVER + "runtime/state_machine/store.rs",
     "r.status IN ('RUNNING', 'VERIFYING') \\", "r.status IN ('RUNNING', 'VERIFYING', 'WAITING_APPROVAL') \\"),
    ("status: a completed Turn counts", "rust-int", SERVER + "runtime/state_machine/store.rs",
     "t.status = 'active' AND", "t.status IN ('active', 'completed') AND"),
    ("status: an unknown outcome counts as in flight", "rust-int", SERVER + "effects/ledger.rs",
     "AND status IN ('RESERVED', 'DISPATCHED', 'RECONCILING') \\", "AND status IN ('RESERVED', 'DISPATCHED', 'RECONCILING', 'OUTCOME_UNKNOWN') \\"),
    ("status: the workspace does not narrow the effect count", "rust-int", SERVER + "effects/ledger.rs",
     "AND status IN ('RESERVED', 'DISPATCHED', 'RECONCILING') \\\n             AND ($2::text IS NULL OR workspace_id = $2)\",",
     "AND status IN ('RESERVED', 'DISPATCHED', 'RECONCILING') \\\n             AND ($2::text IS NULL OR workspace_id = workspace_id)\","),
    ("status: a closed terminal counts", "rust-int", "crates/machine/src/control/terminal.rs",
     "s.status = 'open' \\", "s.status IN ('open', 'closed') \\"),
    ("status: idle ignores the actions in flight", "rust-int", SERVER + "api/maintenance.rs",
     '"idle": active_turns == 0 && inflight_actions == 0,', '"idle": active_turns == 0,'),
    ("status: a malformed workspace id is accepted", "rust-int", SERVER + "api/maintenance.rs",
     'if !workspace.starts_with("ws_") || workspace.len() > 64 {', "if false {"),
    # ---- keep-awake, connectivity, quit, update
    ("keep-awake: held on battery", "vitest", MAIN + "keep-awake.ts",
     "return inputs.running && inputs.onAcPower && inputs.enabled;", "return inputs.running && inputs.enabled;"),
    ("keep-awake: held after the person turned it off", "vitest", MAIN + "keep-awake.ts",
     "return inputs.running && inputs.onAcPower && inputs.enabled;", "return inputs.running && inputs.onAcPower;"),
    ("connectivity: a flap inside the grace reconnects", "vitest", MAIN + "connectivity.ts",
     'if (was === "offline") {', "if (true) {"),
    ("connectivity: signals together reconnect more than once", "vitest", MAIN + "connectivity.ts",
     "if (now - this.#lastReconnect < this.#options.minGapMs) {", "if (false) {"),
    ("connectivity: the offline grace is not held", "vitest", MAIN + "connectivity.ts",
     "        }, this.#options.graceMs);", "        }, 0);"),
    ("quit: running work is read as idle", "vitest", MAIN + "quit.ts",
     'if (status.kind === "idle") {', 'if (status.kind !== "unknown") {'),
    ("quit: a status that cannot be read is read as idle", "vitest", MAIN + "quit.ts",
     '        status = { kind: "unknown" };\n      }\n      if (status.kind === "idle") {', '        status = { kind: "idle" };\n      }\n      if (status.kind === "idle") {'),
    ("quit: overlapping requests each ask", "vitest", MAIN + "quit.ts",
     "    if (this.#asking) {\n      return this.#asking;\n    }\n", ""),
    ("update: installs over running work without asking", "vitest", MAIN + "update-gate.ts",
     'const status = await this.#read();\n    if (status.kind === "idle") {\n      this.#stopWaiting();\n      return this.#install();\n    }\n    const choice',
     'const status = await this.#read();\n    if (status.kind === "idle" || (status as { kind: string }).kind === "busy") {\n      this.#stopWaiting();\n      return this.#install();\n    }\n    const choice'),
    ("update: install-when-idle installs while busy", "vitest", MAIN + "update-gate.ts",
     'if (status.kind === "idle" && this.#options.updates.downloaded) {', "if (this.#options.updates.downloaded) {"),
    ("update: a failed install leaves the permission to quit", "vitest", MAIN + "update-gate.ts",
     "    this.#options.revokeQuit();\n    return { kind: \"failed\" };", "    return { kind: \"failed\" };"),
    ("update: a failed installer leaves install-on-quit on", "vitest", MAIN + "updates.ts",
     "          this.#installing = false;\n          this.setInstallOnQuit(false);\n        }\n        this.#set({ state: \"error\", channel, message: error.message });",
     "          this.#installing = false;\n        }\n        this.#set({ state: \"error\", channel, message: error.message });"),
    ("status: a response that contradicts itself is trusted as idle", "vitest", MAIN + "work-status.ts",
     'if (record["idle"] !== (activeTurns === 0 && inflightActions === 0)) {', "if (false) {"),
    ("status: an unreadable status is idle", "vitest", MAIN + "work-status.ts",
     'return server === null ? { kind: "unknown" } : { kind: "idle" };', 'return { kind: "idle" };'),
    ("dialog: quitting is the default button", "vitest", MAIN + "dialogs.ts",
     'buttons: [t("quitStay"), t("quitConfirm")],\n    defaultId: 0,', 'buttons: [t("quitStay"), t("quitConfirm")],\n    defaultId: 1,'),
    # ---- shortcuts
    ("shortcut: a menu accelerator can be taken", "vitest", MAIN + "shortcuts.ts",
     "if (fixed === binding) {", "if (false) {"),
    ("shortcut: another shortcut's binding can be taken", "vitest", MAIN + "shortcuts.ts",
     "if (other !== id && this.binding(other) === binding) {", "if (false) {"),
    ("shortcut: a declined registration does not restore the previous binding", "vitest", MAIN + "shortcuts.ts",
     "restored = this.#register(id, previousBinding) === null;", "restored = true;"),
    ("shortcut: a binding the person cleared comes back as the default", "vitest", MAIN + "shortcuts.ts",
     "return stored === undefined ? this.#defaultOf(id) : stored;", "return stored ?? this.#defaultOf(id);"),
    ("shortcut: recording leaves the global shortcuts registered", "vitest", MAIN + "shortcuts.ts",
     "      this.#recording = windowId;\n      for (const id of GLOBAL_SHORTCUTS) {\n        this.#release(id);\n      }",
     "      this.#recording = windowId;\n      for (const id of [] as GlobalShortcutId[]) {\n        this.#release(id);\n      }"),
    ("shortcut: recording leaves the menu's bindings", "vitest", MAIN + "shortcuts.ts",
     "out[id] = this.#recording === null ? this.binding(id) : null;", "out[id] = this.binding(id);"),
    ("shortcut: recording does not end when the window loses focus", "vitest", MAIN + "shortcuts.ts",
     "if (this.#recording === windowId) {\n      this.endRecording(event);", "if (false) {\n      this.endRecording(event);"),
    ("grammar: a bare letter is a shortcut", "vitest", DESKTOP + "shortcut-grammar.ts",
     "if (modifiers.length === 0 && !FUNCTION_KEY.test(key)) {", "if (false) {"),
    ("grammar: upper case is accepted", "vitest", DESKTOP + "shortcut-grammar.ts",
     "if (text !== text.toLowerCase() || /\\s/.test(text)) {", "if (false) {"),
    # ---- IPC trust and windows
    ("ipc: an untrusted URL is trusted", "vitest", MAIN + "ipc-trust.ts",
     "if (sender.frameUrl === null || !sender.isMainFrame || !navigationAllowed(sender.frameUrl)) {", "if (sender.frameUrl === null || !sender.isMainFrame) {"),
    ("ipc: a subframe is trusted", "vitest", MAIN + "ipc-trust.ts",
     "if (sender.frameUrl === null || !sender.isMainFrame || !navigationAllowed(sender.frameUrl)) {", "if (sender.frameUrl === null || !navigationAllowed(sender.frameUrl)) {"),
    ("ipc: a window's type does not limit its channels", "vitest", MAIN + "ipc-trust.ts",
     "if (!policy.windows.includes(window.type)) {", "if (!policy.windows.includes(window.type) && policy.windows.length < 0) {"),
    ("ipc: a pop-out is not kept to its own thread", "vitest", MAIN + "ipc-trust.ts",
     'if (window.type === "popout" && policy.popout) {', 'if (window.type === "popout" && policy.popout && policy.windows.length < 0) {'),
    ("ipc: a channel nobody listed is open to every window type", "vitest", MAIN + "ipc-trust.ts",
     'const windows: WindowType[] = ["main"];', 'const windows: WindowType[] = ["main", "popout", "quick_entry", "indicator"];'),
    ("windows: launch data accepts any thread id", "vitest", MAIN + "windows.ts",
     "threadId: s.string({ min: 30, max: 30, pattern: THREAD_ID }),", "threadId: s.string({ max: 100 }),"),
    ("windows: the baseline lets webSecurity off", "vitest", MAIN + "security.ts",
     "  webSecurity: true,", "  webSecurity: false,"),
    ("windows: the pending queue is unbounded", "vitest", MAIN + "windows.ts",
     "if (this.#events.length > this.#cap) {", "if (false) {"),
    ("windows: a closed window keeps its queue", "vitest", MAIN + "windows.ts",
     "          window.pending.clear();\n", ""),
    ("windows: pulling the queue does not mark the page subscribed", "vitest", MAIN + "windows.ts",
     "    window.subscribed = true;\n    return window.pending.drain();", "    return window.pending.drain();"),
    ("windows: the indicator is not click-through", "vitest", MAIN + "windows.ts",
     "    window.handle.setIgnoreMouseEvents(true, true);\n", ""),
    ("windows: a new page keeps the old page's subscription", "vitest", MAIN + "windows.ts",
     "          window.subscribed = false;\n", ""),
    ("indicator: the position is saved under one key for every display", "vitest", MAIN + "shell-host.ts",
     "indicator: { ...previous.indicator, [placed.key]: placed.position }", 'indicator: { ...previous.indicator, ["d0"]: placed.position }'),
    # ---- activity and the host
    ("activity: the groups are in another order", "vitest", MAIN + "activity.ts",
     'const GROUP_ORDER: readonly ActivityGroup[] = ["waiting", "running", "error", "completed"];', 'const GROUP_ORDER: readonly ActivityGroup[] = ["running", "waiting", "error", "completed"];'),
    ("activity: a replay brings back a dismissed item", "vitest", MAIN + "activity.ts",
     "if (this.#dismissed.has(key)) {", "if (false) {"),
    ("activity: an item waiting for the person can be dropped for room", "vitest", MAIN + "activity.ts",
     '.filter((item) => item.group !== "waiting")', ".filter(() => true)"),
    ("activity: a Turn that aborts stays running", "vitest", MAIN + "activity.ts",
     '      case "turn.completed":\n      case "turn.aborted": {', '      case "turn.completed": {'),
    ("host: a refused quick-entry post still hides the window", "vitest", MAIN + "shell-host.ts",
     "          if (result.ok) {\n            window.handle.hide();", "          if (true) {\n            window.handle.hide();"),
    ("host: the connectivity report is dropped", "vitest", MAIN + "shell-host.ts",
     "          o.connectivity.report(input.value.online);", "          void input.value.online;"),
    ("host: turning keep-awake off does not release the blocker", "vitest", MAIN + "shell-host.ts",
     "            o.keepAwake.update({ enabled: change.keepAwake });", "            void change.keepAwake;"),
    ("host: any window can end another window's recording", "vitest", MAIN + "shell-host.ts",
     'o.shortcuts.recordingWindow() === window.handle.webContentsId ? o.shortcuts.endRecording("done") : false', 'o.shortcuts.endRecording("done")'),
    ("host: indicator hides never", "vitest", MAIN + "shell-host.ts",
     "    } else if (!show && indicator.handle.isVisible()) {\n      indicator.handle.hide();", "    } else if (!show && indicator.handle.isVisible() && show) {\n      indicator.handle.hide();"),
    ("menu: the recording menu keeps its roles and accelerators", "vitest", MAIN + "menu.ts",
     "if (state.paused === true) {\n    // A role", "if (false) {\n    // A role"),
    ("tray: a window with no tray is hidden where it cannot be brought back", "vitest", MAIN + "tray.ts",
     'return input.trayAvailable ? "hide" : "minimize";', 'return "hide";'),
    ("bridge: a focus signal reconnects an open stream", "vitest", MAIN + "ipc.ts",
     'if (!force && this.#streamState === "open") {', "if (false) {"),
    ("config: the queue cap has no upper bound", "vitest", MAIN + "shell-config.ts",
     '"pending.queue_cap": [1, 256],', '"pending.queue_cap": [1, 100000],'),
    ("schema: an unknown field passes through a payload", "vitest", MAIN + "schema.ts",
     "        if (!known.has(key)) {", "        if (false) {"),
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

status = subprocess.run(["git", "status", "--porcelain"], cwd=REPO, capture_output=True, text=True).stdout
dirty = [line for line in status.splitlines() if "node_modules" not in line]
if dirty:
    sys.exit("the tree must be clean and committed before mutating:\n" + "\n".join(dirty))

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
                log(out_of(build)[-600:])
                no_verdict += 1
                continue
            log(f"COMPILES    {name}")
        result = run(test_cmd, cwd)
        failed = sorted(set(failing.findall(out_of(result))))
        failed = [f if isinstance(f, str) else " ".join(f) for f in failed]
        if result.returncode != 0 and failed:
            log(f"KILLED      {name} -- {failed[0]}" + (f" (+{len(failed) - 1} more)" if len(failed) > 1 else ""))
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
