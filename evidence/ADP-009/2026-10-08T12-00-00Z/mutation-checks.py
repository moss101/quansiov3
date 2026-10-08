#!/usr/bin/env python3
"""ADP-009 mutation checks. Usage: mutation-checks.py WORKTREE LOG [name ...]

Each mutant: run the CONTROL (the same test command, unmutated; no verdict when it fails), apply the edit, build (a mutant that does
not compile is NOT_COMPILED, never KILLED), run the tests (KILLED when they fail, SURVIVED when they pass), restore the file from
a byte copy and rebuild. A test run is bounded by a timeout so a mutant that removes a deadline cannot hang the run.
"""
import os
import shutil
import subprocess
import sys

W, LOG = sys.argv[1], sys.argv[2]
ONLY = set(sys.argv[3:])
S = "/private/tmp/claude-501/-Users-mohsin-projects-quansiov3/66f285fe-b59d-4c10-87a8-fe7310b75e93/scratchpad"
CARGO = f"{S}/cargo9.sh"

M = "crates/machine/src/remote/session.rs"
P = "crates/machine/src/remote/protocol.rs"
H = "crates/server/src/api/remote.rs"
Q = "crates/qworkerd/src/remote.rs"
MACHINE = ([CARGO, "test", "-p", "quansio-machine", "--lib", "remote"], [CARGO, "build", "-q", "-p", "quansio-machine", "--lib"])
SERVER_RC = ([CARGO, "test", "-p", "quansio-server", "--test", "remote_control"], [CARGO, "build", "-q", "-p", "quansio-server", "--tests"])
HOST = ([CARGO, "test", "-p", "quansio-qworkerd", "--test", "remote_host"], [CARGO, "build", "-q", "-p", "quansio-qworkerd", "--tests"])
VITEST = lambda *files: (["npx", "vitest", "run", "--root", "apps/desktop", *files], ["true"])

MUTANTS = [
    ("remote-view-grant-takes-input", M, "        if self.granted != Some(Scope::Control) {\n            return Err(Refusal::ViewOnly);\n        }\n        self.ready(config, now_ms, true)?;\n        let Some((_, width, height))", "        self.ready(config, now_ms, true)?;\n        let Some((_, width, height))", MACHINE),
    ("remote-locked-screen-takes-input", M, "        if !conditions.unlocked {", "        if false {", MACHINE),
    ("remote-stale-report-admitted", M, "        if now_ms - at > config.host_report_max_age_seconds * 1000", "        if false && now_ms - at > config.host_report_max_age_seconds * 1000", MACHINE),
    ("remote-anyone-decides", M, "        if by.user_id != self.computer.owner_user_id {", "        if false {", MACHINE),
    ("remote-grant-widens-request", M, "if scope > self.requested ||", "if false ||", MACHINE),
    ("remote-no-idle-end", M, "                if now_ms - self.last_activity_ms >= config.session_idle_seconds * 1000 =>", "                if false =>", MACHINE),
    ("remote-clipboard-name-unchecked", P, ".any(|c| c.is_control() || matches!(c, '/' | '\\\\' | ':' | '\\u{0}'))", ".any(|_| false)", MACHINE),
    ("remote-clipboard-total-uncapped", P, "if bytes.is_empty() || total > config.clipboard_max_bytes {", "if bytes.is_empty() {", MACHINE),
    ("remote-second-session-allowed", H, "                busy = true;", "                busy = false;", SERVER_RC),
    ("remote-frames-to-everyone", H, "\"remote.frame\" => session.viewer_receives(who),", "\"remote.frame\" => true,", SERVER_RC),
    ("remote-clipboard-to-wrong-side", H, "Some(\"to_host\") => session.host_receives(who),", "Some(\"to_host\") => session.viewer_receives(who),", SERVER_RC),
    ("remote-agent-fence-dropped", "crates/server/src/api/remote.rs", "if matches!(control, Ok(ref control) if matches!(control.holder, MachineHolder::Agent)) {", "if false {", SERVER_RC),
    ("remote-handback-keeps-sessions", "crates/server/src/api/live_sessions.rs", "super::remote::agent_resumed(state, &scope.tenant_id, &target_id).await;", "let _ = &target_id;", SERVER_RC),
    ("remote-host-skips-own-readiness", Q, "        self.report(now_ms)?;\n        let viewer = self.session.viewer().clone();\n        self.session.admit_input", "        let viewer = self.session.viewer().clone();\n        self.session.admit_input", HOST),
    ("remote-host-trusts-server-viewer", Q, "            != Some(self.session.viewer().user_id.as_str())\n        {\n            return Err(Refusal::WrongParty);", "            != Some(self.session.viewer().user_id.as_str()) && false\n        {\n            return Err(Refusal::WrongParty);", HOST),
    ("remote-indicator-dismissible", "apps/desktop/src/main/activity.ts", "if (this.#items.get(key) !== undefined && isRemoteKind(this.#items.get(key)?.kind ?? \"\")) {", "if (false) {", VITEST("tests/shell-host.test.ts", "tests/shell-activity.test.ts")),
    ("remote-indicator-shown-to-viewer", "apps/desktop/src/main/activity.ts", "if (self !== null && self !== host) {", "if (false) {", VITEST("tests/shell-activity.test.ts")),
    ("remote-indicator-answer-unfenced", "apps/desktop/src/main/shell-host.ts", "if (asking !== (answer !== \"stop\") || (answer === \"control\" && item.kind !== \"remote_ask_control\")) {", "if (false) {", VITEST("tests/shell-host.test.ts")),
    ("remote-indicator-outlives-session", "apps/desktop/src/main/activity.ts", "if (isRemoteKind(item.kind) && item.ref.untilMs !== undefined && item.ref.untilMs <= now) {", "if (false) {", VITEST("tests/shell-activity.test.ts")),
    ("payment-origin-unchecked", "crates/qworkerd/src/browser/actions/payment.rs", "if !origin_allowed(origin, std::slice::from_ref(&payment.allowed_origin)) {", "if false {", ([CARGO, "test", "-p", "quansio-qworkerd", "--test", "browser_payment"], [CARGO, "build", "-q", "-p", "quansio-qworkerd", "--tests"])),
    ("payment-expired-value-taken", "crates/machine/src/secrets/payment.rs", "AND label LIKE $4 AND (expires_at IS NULL OR expires_at > $5::timestamptz)", "AND label LIKE $4 AND (true OR expires_at > $5::timestamptz)", ([CARGO, "test", "-p", "quansio-server", "--test", "payment_flow", "--test", "payment_supply"], [CARGO, "build", "-q", "-p", "quansio-server", "--tests"])),
    ("import-staging-needs-no-tier4", "crates/server/src/api/browser_imports.rs", "ActionFamily::ExecuteEffect { tier: 4 },\n        correlation,\n    )\n    .await?;\n    let Some(keys)", "ActionFamily::ExecuteEffect { tier: 1 },\n        correlation,\n    )\n    .await?;\n    let Some(keys)", ([CARGO, "test", "-p", "quansio-server", "--test", "browser_import"], [CARGO, "build", "-q", "-p", "quansio-server", "--tests"])),
    ("sandbox-deadline-ignored", "crates/qworkerd/src/compose/engine.rs", "self.aborted.load(Ordering::SeqCst) || Instant::now() >= self.deadline", "false", ([CARGO, "test", "-p", "quansio-qworkerd", "--test", "compose"], [CARGO, "build", "-q", "-p", "quansio-qworkerd", "--tests"])),
    ("mcp-shadows-builtin-name", "crates/tools/src/registry.rs", ".any(|d| d.name == declaration.name && matches!(d.origin, ToolOrigin::Builtin))", ".any(|_| false)", ([CARGO, "test", "-p", "quansio-tools", "--test", "mcp_registry"], [CARGO, "build", "-q", "-p", "quansio-tools", "--tests"])),
    ("mcp-external-trusted", "crates/tools/src/registry.rs", "declaration.source_trust != crate::declaration::SourceTrust::UntrustedExternal", "false", ([CARGO, "test", "-p", "quansio-tools", "--test", "mcp_registry"], [CARGO, "build", "-q", "-p", "quansio-tools", "--tests"])),
    ("vault-saves-in-plaintext", "apps/desktop/src/main/secrets/vault.ts", "    if (!this.available()) {\n      throw new VaultError(\"unavailable\", \"the platform can not encrypt, so nothing is saved\");", "    if (false) {\n      throw new VaultError(\"unavailable\", \"the platform can not encrypt, so nothing is saved\");", VITEST("tests/secrets-vault.test.ts")),
]


def log(line):
    with open(LOG, "a", encoding="utf-8") as handle:
        handle.write(line + "\n")
    print(line, flush=True)


def run(cmd, timeout):
    try:
        done = subprocess.run(cmd, cwd=W, capture_output=True, text=True, timeout=timeout)
        return done.returncode
    except subprocess.TimeoutExpired:
        return 124


controls = {}
for name, path, old, new, (test, build) in MUTANTS:
    if ONLY and name not in ONLY:
        continue
    full = os.path.join(W, path)
    original = open(full, "rb").read()
    text = original.decode()
    if text.count(old) != 1:
        log(f"{name}: PATTERN_NOT_UNIQUE ({text.count(old)})")
        continue
    key = tuple(test)
    if key not in controls:
        controls[key] = run(test, 900)
    if controls[key] != 0:
        log(f"{name}: CONTROL-FAILED rc={controls[key]} (no verdict)")
        continue
    log(f"{name}: CONTROL-PASSED")
    try:
        open(full, "w", encoding="utf-8").write(text.replace(old, new))
        if build != ["true"]:
            brc = run(build, 900)
            if brc != 0:
                log(f"{name}: NOT_COMPILED (no verdict)")
                continue
            log(f"{name}: COMPILES")
        rc = run(test, 900)
        log(f"{name}: {'TIMEOUT-KILLED' if rc == 124 else 'KILLED' if rc != 0 else 'SURVIVED'} (rc={rc})")
    finally:
        open(full, "wb").write(original)
        if build != ["true"]:
            run(build, 900)
