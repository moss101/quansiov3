#!/usr/bin/env python3
"""ADP-001 mutation checks: each guard is broken on purpose and the suite that claims to prove it must fail.

A mutant counts as KILLED only if (1) it compiles (`COMPILES`), (2) the unmutated suite passed first
(`CONTROL-PASSED`) and (3) the suite then fails. A mutant that does not compile is NO-VERDICT, never
KILLED (memory: mutation-checks-must-compile). Files are restored from a byte-for-byte copy taken before
the run, and the test bundle is rebuilt after every restore.
"""
import pathlib
import subprocess
import sys
import shutil

ROOT = pathlib.Path("/Users/mohsin/projects/quansiov3/.claude/worktrees/agent-a89e68c0f39d045d2")
SCRATCH = pathlib.Path("/private/tmp/claude-501/-Users-mohsin-projects-quansiov3/66f285fe-b59d-4c10-87a8-fe7310b75e93/scratchpad")
BACKUP = SCRATCH / "mutation-backup"
OUT = SCRATCH / "adp001-mutations.log"

Q = "crates/qworkerd/src/browser/"
MUTANTS = [
    # name, file, old, new, suite (cargo test args)
    ("stale-revision-check-disabled", Q + "driver.rs",
     "        if state.revision != expected {\n            return Err(stale(\n                \"document_revision\",",
     "        if false && state.revision != expected {\n            return Err(stale(\n                \"document_revision\",",
     ["-p", "quansio-qworkerd", "--test", "browser_fencing"]),
    ("fingerprint-difference-ignored", Q + "fence.rs",
     "        if self.tag != live.tag {\n            Some(\"tag\")",
     "        if true {\n            None\n        } else if self.tag != live.tag {\n            Some(\"tag\")",
     ["-p", "quansio-qworkerd", "--test", "browser_fencing"]),
    ("hit-test-skipped", Q + "driver.rs",
     "                if !contains {\n                    return Err(stale(\n                        \"hit_test_mismatch\",",
     "                if false && !contains {\n                    return Err(stale(\n                        \"hit_test_mismatch\",",
     ["-p", "quansio-qworkerd", "--test", "browser_fencing"]),
    ("observations-not-invalidated-by-a-mutation", Q + "tab.rs",
     "    pub fn invalidate_observations(&mut self) {\n        self.snapshots.clear();\n        self.captures.clear();\n    }",
     "    pub fn invalidate_observations(&mut self) {}",
     ["-p", "quansio-qworkerd", "--test", "browser_fencing"]),
    ("scroll-offset-not-checked", Q + "driver.rs",
     "                if scroll_x != capture.scroll_x || scroll_y != capture.scroll_y {",
     "                if false && (scroll_x != capture.scroll_x || scroll_y != capture.scroll_y) {",
     ["-p", "quansio-qworkerd", "--test", "browser_fencing"]),
    ("pixel-change-warning-dropped", Q + "driver.rs",
     "                if changed > 0 {\n                    warnings.push(",
     "                if false && changed > 0 {\n                    warnings.push(",
     ["-p", "quansio-qworkerd", "--test", "browser_fencing"]),
    ("replay-gate-always-fresh", Q + "actions.rs",
     "        Ok(match self.inner.journal.check(key, digest)? {\n            Begin::Fresh => None,",
     "        Ok(match { let _ = self.inner.journal.check(key, digest)?; Begin::Fresh } {\n            Begin::Fresh => None,",
     ["-p", "quansio-qworkerd", "--test", "browser_ledger"]),
    ("journal-digest-conflict-ignored", Q + "journal.rs",
     "            if entry.digest != digest {",
     "            if false && entry.digest != digest {",
     ["-p", "quansio-qworkerd", "--test", "browser_ledger"]),
    ("sent-flag-never-set-so-unacknowledged-input-is-a-plain-failure", Q + "actions.rs",
     "                if step.marks_sent {\n                    sent.store(true, Ordering::SeqCst);\n                }",
     "                let _ = step.marks_sent;",
     ["-p", "quansio-qworkerd", "--test", "browser_ledger"]),
    ("restart-does-not-mark-started-actions-in-doubt", Q + "journal.rs",
     "        for key in doubtful {\n            journal.mark_in_doubt(",
     "        for key in doubtful.into_iter().filter(|_| false) {\n            journal.mark_in_doubt(",
     ["-p", "quansio-qworkerd", "--test", "browser_ledger"]),
    ("click-takes-no-queue-turn", Q + "actions.rs",
     "        let _turn = self.inner.queue.lock().await;\n        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);\n        let digest = digest_of(\n            \"browser.click\",",
     "        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);\n        let digest = digest_of(\n            \"browser.click\",",
     ["-p", "quansio-qworkerd", "--test", "browser_ledger"]),
    ("tier-ceiling-ignored", Q + "actions.rs",
     "            if *tier > ceiling {",
     "            if false && *tier > ceiling {",
     ["-p", "quansio-qworkerd", "--test", "browser_ledger"]),
    ("plain-text-allowed-in-sensitive-fields", Q + "actions.rs",
     "        if let Some(kind) = sensitive {\n            if !(uses_secrets && only_placeholders(&segments)) {",
     "        if let Some(kind) = sensitive {\n            if false && !(uses_secrets && only_placeholders(&segments)) {",
     ["-p", "quansio-qworkerd", "--test", "browser_secrets"]),
    ("secret-origin-not-checked", Q + "actions.rs",
     "                        if !origin_allowed(origin, &resolved_secret.allowed_origins) {",
     "                        if false && !origin_allowed(origin, &resolved_secret.allowed_origins) {",
     ["-p", "quansio-qworkerd", "--test", "browser_secrets"]),
    ("target-frame-origin-not-checked", Q + "actions.rs",
     "                    for (which, origin) in [(\"top\", &top_origin), (\"target_frame\", &frame_origin)] {",
     "                    for (which, origin) in [(\"top\", &top_origin)] {",
     ["-p", "quansio-qworkerd", "--test", "browser_secrets"]),
    ("printable-key-allowed-in-sensitive-fields", Q + "actions.rs",
     "            if cross_origin || sensitivity_of(&descriptor).is_some() {",
     "            if false && (cross_origin || sensitivity_of(&descriptor).is_some()) {",
     ["-p", "quansio-qworkerd", "--test", "browser_secrets"]),
    ("digest-over-the-substituted-text", Q + "actions.rs",
     "                \"text\": text,\n                \"clear\": clear,\n            }),\n        );\n        if let Some(report) = self.replay_gate(&key, &digest)? {\n            return Ok(report);\n        }\n        self.require_control()?;\n        if uses_secrets",
     "                \"text\": text.replace(\"{{secret:acme.password}}\", \"hunter2-CANARY-7731\"),\n                \"clear\": clear,\n            }),\n        );\n        if let Some(report) = self.replay_gate(&key, &digest)? {\n            return Ok(report);\n        }\n        self.require_control()?;\n        if uses_secrets",
     ["-p", "quansio-qworkerd", "--test", "browser_secrets"]),
    ("pending-dialog-does-not-fence", Q + "driver.rs",
     "        if !allow_dialog {\n            if let Some(dialog) = &state.dialog {",
     "        if false && !allow_dialog {\n            if let Some(dialog) = &state.dialog {",
     ["-p", "quansio-qworkerd", "--test", "browser_guard"]),
    ("popups-from-agent-input-allowed", Q + "driver.rs",
     "    let genuine = tab\n        .state()\n        .user_gesture_within(driver.config.gesture_window);\n    for page in pages",
     "    let genuine = true;\n    for page in pages",
     ["-p", "quansio-qworkerd", "--test", "browser_guard"]),
    ("stale-dialog-id-not-checked", Q + "actions.rs",
     "                Some(dialog) if dialog.id != dialog_id => return Err(stale_dialog(\"dialog_id\")),",
     "                Some(dialog) if false && dialog.id != dialog_id => return Err(stale_dialog(\"dialog_id\")),",
     ["-p", "quansio-qworkerd", "--test", "browser_guard"]),
    ("certificate-errors-continue-without-a-user", Q + "tab.rs",
     "                    status: if navigating {\n                        CertificateStatus::Pending",
     "                    status: if false {\n                        CertificateStatus::Pending",
     ["-p", "quansio-qworkerd", "--test", "browser_guard"]),
    ("permissions-not-denied-at-attach", Q + "driver.rs",
     "        for permission in DENIED_PERMISSIONS {",
     "        for permission in DENIED_PERMISSIONS.iter().take(0).copied() {",
     ["-p", "quansio-qworkerd", "--test", "browser_guard"]),
    ("navigation-validation-disabled", Q + "origin.rs",
     "    if url == \"about:blank\" {\n        return Ok(());\n    }",
     "    if true {\n        return Ok(());\n    }",
     ["-p", "quansio-qworkerd", "--test", "browser_guard"]),
    ("tool-output-not-schema-validated", Q + "tools.rs",
     "        if let Some(violation) = first_violation(output_schema, &report.output) {",
     "        if let Some(violation) = first_violation(output_schema, &report.output).filter(|_| false) {",
     ["-p", "quansio-qworkerd", "--lib"]),
    ("tool-input-not-schema-validated", Q + "tools.rs",
     "        if let Some(violation) = first_violation(&declaration.input_schema, args) {",
     "        if let Some(violation) = first_violation(&declaration.input_schema, args).filter(|_| false) {",
     ["-p", "quansio-qworkerd", "--test", "browser_guard"]),
    ("side-effect-class-not-checked-against-mapping", "crates/tools/src/side_effect.rs",
     "            crate::declaration::EffectClassRule::Static(declared) if declared == expected => Ok(()),",
     "            crate::declaration::EffectClassRule::Static(_) => Ok(()),",
     ["-p", "quansio-tools", "--test", "browser_side_effects"]),
    ("an-agent-can-make-a-user-decision", "crates/machine/src/control/browser_guard.rs",
     "        DecisionActor::User { user_id } if user_id.starts_with(\"usr_\") => Ok(user_id.clone()),",
     "        DecisionActor::User { user_id } => Ok(user_id.clone()),\n        DecisionActor::Agent { identity } => Ok(identity.clone()),",
     ["PG", "-p", "quansio-machine", "--test", "browser_guard"]),
    ("a-report-can-undo-a-users-decision", "crates/machine/src/control/browser_guard.rs",
     "                    .filter(|stored| stored.status != \"pending\")",
     "                    .filter(|_| false)",
     ["PG", "-p", "quansio-machine", "--test", "browser_guard"]),
    ("a-secret-resolves-without-a-matching-label", "crates/machine/src/control/browser_guard.rs",
     "            handle.label == request.name\n                && ",
     "            true\n                && ",
     ["PG", "-p", "quansio-machine", "--test", "browser_guard"]),
]


def log(line):
    with OUT.open("a") as fh:
        fh.write(line + "\n")
    print(line, flush=True)


def run(args, timeout=900):
    if args and args[0] == "test" and len(args) > 1 and args[1] == "PG":
        return subprocess.run(["bash", str(SCRATCH / "pgtest.sh"), "cargo", "test", *args[2:]], cwd=ROOT, capture_output=True, text=True, timeout=timeout)
    return subprocess.run(["cargo", *args], cwd=ROOT, capture_output=True, text=True, timeout=timeout)


def main():
    only = set(sys.argv[1:])
    BACKUP.mkdir(exist_ok=True)
    OUT.write_text("")
    controls = {}
    for name, rel, old, new, suite in MUTANTS:
        if only and name not in only:
            continue
        path = ROOT / rel
        original = path.read_bytes()
        backup = BACKUP / (rel.replace("/", "__") + ".orig")
        if not backup.exists():
            backup.write_bytes(original)
        text = original.decode()
        if text.count(old) != 1:
            log(f"{name}: NO-VERDICT (pattern found {text.count(old)} times in {rel})")
            continue
        key = tuple(suite)
        if key not in controls:
            control = run(["test", *suite])
            controls[key] = control.returncode == 0
            log(f"CONTROL {' '.join(suite)}: {'CONTROL-PASSED' if controls[key] else 'CONTROL-FAILED'}")
        if not controls[key]:
            log(f"{name}: NO-VERDICT (the unmutated suite does not pass)")
            continue
        try:
            path.write_text(text.replace(old, new))
            built = run(["test", *suite, "--no-run"])
            if built.returncode != 0:
                log(f"{name}: NO-VERDICT (does not compile)")
                continue
            log(f"COMPILES {name}")
            result = run(["test", *suite])
            failed = [line.split()[1] for line in result.stdout.splitlines() if line.startswith("test ") and line.endswith("FAILED")]
            if result.returncode != 0 and failed:
                log(f"{name}: KILLED by {', '.join(failed)}")
            elif result.returncode != 0:
                log(f"{name}: NO-VERDICT (suite failed without a failing test: harness error)")
            else:
                log(f"{name}: SURVIVED")
        finally:
            path.write_bytes(original)
    # Rebuild once after the last restore so the bundle matches the source again.
    run(["test", "-p", "quansio-qworkerd", "--no-run"])
    log("restored")


main()
