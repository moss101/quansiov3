#!/usr/bin/env python3
"""ADP-008 mutation checks: break each guard on purpose and prove a named test then fails.

A mutant is KILLED only if it COMPILES, its CONTROL (the same test command, unmutated) passed, and the test command
then fails. A mutant that does not compile is NO-VERDICT, never KILLED. Sources are restored from memory after every
mutant; the script is run from the repository root with the private test environment exported (see NOTES.md).

    python3 evidence/ADP-008/mutation-checks.py [name ...]  > mutation-checks.log
"""

from __future__ import annotations

import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path.cwd()
TOOLS = ["cargo", "test", "-p", "quansio-tools", "--test", "visibility", "-q"]
SERVER = lambda test, *extra: ["cargo", "test", "-p", "quansio-server", "--test", test, *extra]  # noqa: E731
PY = lambda *tests: ["uv", "run", "--project", "python", "pytest", "-q", "-p", "no:cacheprovider", *tests]  # noqa: E731


@dataclass
class Mutant:
    name: str
    path: str
    old: str
    new: str
    test: list[str]
    build: list[str]
    why: str


CARGO_TOOLS = ["cargo", "build", "-q", "-p", "quansio-tools", "--tests"]
CARGO_SERVER = ["cargo", "build", "-q", "-p", "quansio-server", "--tests"]
PY_IMPORT = ["uv", "run", "--project", "python", "python", "-c", "import intelligence.capability_compiler.install, intelligence.model_gateway.tooling"]
PACKS_TEST = PY("python/tests/capability_compiler/test_pack_install.py")
VIS_PY = PY("python/tests/intelligence/test_tool_visibility.py")

MUTANTS = [
    Mutant("registry-ignores-model-visible", "crates/tools/src/registry.rs",
           "declaration.model_visible && declaration.covered_by(&projection.grants, now)",
           "declaration.covered_by(&projection.grants, now)", TOOLS, CARGO_TOOLS,
           "the model-facing exposure no longer excludes a host-only tool"),
    Mutant("model-visible-names-lists-all", "crates/tools/src/registry.rs",
           "            .latest()\n            .filter(|declaration| declaration.model_visible)",
           "            .latest()\n            .filter(|_| true)", TOOLS, CARGO_TOOLS,
           "the ceiling for model-facing lists includes host-only tools"),
    Mutant("older-version-makes-name-visible", "crates/tools/src/registry.rs",
           "        self.declarations.iter().filter(|declaration| {\n            self.get(&declaration.name)\n                .is_some_and(|newest| newest.version == declaration.version)\n        })",
           "        self.declarations.iter().filter(|_| true)", TOOLS, CARGO_TOOLS,
           "an older version decides visibility"),
    Mutant("plan-call-skips-the-caller-check", "crates/tools/src/call.rs",
           "    if !declaration.callable_by(context.caller) {", "    if false && !declaration.callable_by(context.caller) {", TOOLS, CARGO_TOOLS,
           "a model can propose a host-only tool"),
    Mutant("default-caller-is-the-host", "crates/tools/src/call.rs",
           "    #[default]\n    Model,\n    /// The trusted runtime, or the person through their own surface, made the call.\n    Host,",
           "    Model,\n    /// The trusted runtime, or the person through their own surface, made the call.\n    #[default]\n    Host,", TOOLS, CARGO_TOOLS,
           "a caller that forgets to say who it is reaches host-only tools"),
    Mutant("unknown-declaration-keys-accepted", "crates/tools/src/declaration.rs",
           "#[serde(deny_unknown_fields)]\npub struct RawToolDeclaration", "pub struct RawToolDeclaration", TOOLS, CARGO_TOOLS,
           "a misspelt flag leaves a tool visible"),
    Mutant("tool-nobody-can-call-accepted", "crates/tools/src/declaration.rs",
           "        if !raw.model_visible && !raw.host_callable {", "        if false && !raw.model_visible && !raw.host_callable {", TOOLS, CARGO_TOOLS,
           "a tool that nobody can call is accepted"),
    Mutant("gateway-offers-host-only-schemas", "python/intelligence/model_gateway/tooling.py",
           "            if not visible:\n                continue\n", "", VIS_PY, PY_IMPORT,
           "a provider is sent a host-only tool's schema"),
    Mutant("recording-compile-lists-every-tool", "crates/server/src/control/skills/recording/proposal.rs",
           "registry.model_visible_names()", "registry.callable_names()", TOOLS, CARGO_SERVER,
           "a recording compile is told about host-only tools"),
    Mutant("host-call-recorded-and-dispatched-as-model", "crates/server/src/runtime/turn_loop/dispatch.rs",
           "let context = self.call_context(&run, call.caller).await?;", "let context = self.call_context(&run, Caller::Model).await?;",
           SERVER("tool_visibility"), CARGO_SERVER, "a host call is treated as a model's"),
    Mutant("rejected-model-attempt-not-recorded-as-model", "crates/server/src/runtime/turn_loop/dispatch.rs",
           "                call.caller,\n            )\n            .await?;\n        self.emit_tool_event(\n            run,\n            &id,\n            \"tool.rejected\",",
           "                Caller::Host,\n            )\n            .await?;\n        self.emit_tool_event(\n            run,\n            &id,\n            \"tool.rejected\",",
           SERVER("tool_visibility"), CARGO_SERVER, "a refused model attempt is recorded under the wrong caller"),
    Mutant("pack-compile-allows-host-only-tools", "python/intelligence/capability_compiler/install.py",
           "        elif not tool.model_visible:", "        elif False:", PACKS_TEST, PY_IMPORT,
           "a pack can name a host-only tool"),
    Mutant("pack-credentials-not-scanned", "python/intelligence/capability_compiler/install.py",
           "    out.extend(_credential_findings(manifest, files))", "    pass", PACKS_TEST, PY_IMPORT,
           "a pack carrying a credential installs"),
    Mutant("revision-includes-the-governance-record", "python/intelligence/capability_compiler/install.py",
           "    content = {key: value for key, value in manifest.items() if key not in GOVERNANCE}",
           "    content = dict(manifest.items())", PACKS_TEST, PY_IMPORT, "a re-review changes the revision"),
    Mutant("revision-ignores-files", "python/intelligence/capability_compiler/install.py",
           '    content = {"files": dict(files), "manifest": content_manifest(manifest)}',
           '    content = {"files": {}, "manifest": content_manifest(manifest)}', PACKS_TEST, PY_IMPORT, "a changed file is the same revision"),
    Mutant("runtime-revision-includes-governance", "crates/server/src/control/packs/content.rs",
           "            .filter(|(key, _)| !GOVERNANCE.contains(&key.as_str()))", "            .filter(|_| true)",
           ["cargo", "test", "-p", "quansio-server", "--lib", "control::packs", "-q"], CARGO_SERVER, "the runtime's revision drifts from the plane's"),
    Mutant("install-ignores-the-plane-verdict", "crates/server/src/control/packs/install.rs",
           "        if !compilation.ok {", "        if false && !compilation.ok {", SERVER("pack_install"), CARGO_SERVER, "a pack the plane refused is activated"),
    Mutant("activation-ignores-the-expected-active-revision", "crates/server/src/control/packs/store.rs",
           "                        if expected != &current_revision {\n                            return Err(PackError::Conflict {\n                                pack: name.clone(),",
           "                        if false && expected != &current_revision {\n                            return Err(PackError::Conflict {\n                                pack: name.clone(),",
           SERVER("pack_install"), CARGO_SERVER, "a concurrent install overwrites the other's revision"),
    Mutant("retention-is-unbounded", "crates/server/src/control/packs/store.rs",
           "        let retain =\n            i64::try_from(self.config.install.retain_previous_revisions).unwrap_or(i64::MAX);",
           "        let retain = i64::MAX;", SERVER("pack_install"), CARGO_SERVER, "history is kept without bound"),
    Mutant("previous-revision-not-retired", "crates/server/src/control/packs/store.rs",
           "                                \"UPDATE capability_pack_versions SET status = 'deprecated', retired_at = now() \\\n                                 WHERE id = $1\",\n                            )\n                            .bind(current)\n                            .execute(&mut **tx)\n                            .await?;\n                        }\n                        let version_id = match existing {",
           "                                \"UPDATE capability_pack_versions SET retired_at = retired_at \\\n                                 WHERE id = $1\",\n                            )\n                            .bind(current)\n                            .execute(&mut **tx)\n                            .await?;\n                        }\n                        let version_id = match existing {",
           SERVER("pack_install"), CARGO_SERVER, "the replaced revision is not kept for rollback"),
    Mutant("an-expired-report-is-read-by-id", "crates/server/src/control/packs/store.rs",
           "WHERE tenant_id = $1 AND request_id = $2 AND expires_at > now()\"",
           "WHERE tenant_id = $1 AND request_id = $2\"", SERVER("pack_install"), CARGO_SERVER, "an expired report is returned by its id"),
    Mutant("expired-reports-are-not-purged", "crates/server/src/control/packs/store.rs",
           "            \"DELETE FROM pack_install_reports WHERE tenant_id = $1 AND expires_at <= now()\",",
           "            \"DELETE FROM pack_install_reports WHERE tenant_id = $1 AND expires_at <= now() AND false\",", SERVER("pack_install"), CARGO_SERVER, "an expired report is kept"),
    Mutant("stale-running-reports-are-not-closed", "crates/server/src/control/packs/store.rs",
           "WHERE r.tenant_id = $1 AND r.state = 'running' \\", "WHERE r.tenant_id = $1 AND r.state = 'running' AND false \\",
           SERVER("pack_install"), CARGO_SERVER, "a crashed install's report stays running"),
    Mutant("diagnostics-are-not-redacted", "crates/server/src/control/packs/report.rs",
           "                &redact_diagnostic(&self.message),\n                limits.max_diagnostic_bytes,",
           "                &self.message,\n                limits.max_diagnostic_bytes,", SERVER("pack_install"), CARGO_SERVER, "a secret reaches the stored report"),
    Mutant("pointers-are-not-redacted", "crates/server/src/control/packs/report.rs",
           ".map(|pointer| bound(&redact_diagnostic(pointer), 256)),", ".map(|pointer| bound(pointer, 256)),", SERVER("pack_install"), CARGO_SERVER,
           "a secret in a quoted path reaches the stored report"),
    Mutant("home-directories-are-kept", "crates/server/src/control/packs/redact.rs",
           "    out = HOME_UNIX.replace_all(&out, HOME_MARKER).into_owned();", "", SERVER("pack_install"), CARGO_SERVER, "a person's name reaches the report"),
    Mutant("url-credentials-are-kept", "crates/server/src/control/packs/redact.rs",
           "    let mut out = URL_USERINFO", "    let mut out = QUERY_SECRET\n        .replace_all(text, \"$0\")\n        .into_owned();\n    let _unused = URL_USERINFO", SERVER("pack_install"), CARGO_SERVER,
           "a URL's credentials reach the report"),
    Mutant("diagnostics-are-not-bounded-in-count", "crates/server/src/control/packs/report.rs",
           "        .take(limits.max_diagnostics)", "        .take(usize::MAX)", SERVER("pack_install"), CARGO_SERVER, "a report is unbounded"),
    Mutant("a-name-the-person-chose-is-stored", "crates/server/src/control/packs/content.rs",
           "    if valid_name(name) {\n        name.to_string()", "    if true {\n        name.to_string()", SERVER("pack_install"), CARGO_SERVER, "text chosen as a name reaches the report"),
    Mutant("reports-are-readable-by-members", "crates/server/src/api/packs.rs",
           "    .is_ok();\n    let reports = if admin {", "    .is_ok();\n    let admin = true;\n    let reports = if admin {", SERVER("pack_install"), CARGO_SERVER,
           "install reports are shown to a member"),
    Mutant("the-plane-may-say-ok-with-an-error", "crates/server/src/control/packs/compile.rs",
           "    if ok == has_error {", "    if false && ok == has_error {",
           ["cargo", "test", "-p", "quansio-server", "--lib", "control::packs::compile", "-q"], CARGO_SERVER, "an incoherent plane answer is believed"),
]


def run(command: list[str]) -> tuple[int, str]:
    done = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    return done.returncode, (done.stdout + done.stderr)[-4000:]


def main(argv: list[str]) -> int:
    wanted = set(argv)
    controls: dict[tuple[str, ...], bool] = {}
    for mutant in MUTANTS:
        if wanted and mutant.name not in wanted:
            continue
        path = ROOT / mutant.path
        original = path.read_text(encoding="utf-8")
        if original.count(mutant.old) != 1:
            print(f"NO-VERDICT {mutant.name}: pattern found {original.count(mutant.old)} times")
            continue
        key = tuple(mutant.test)
        if key not in controls:
            code, _ = run(mutant.test)
            controls[key] = code == 0
            print(f"{'CONTROL-PASSED' if code == 0 else 'CONTROL-FAILED'} {' '.join(mutant.test[:8])}", flush=True)
        if not controls[key]:
            print(f"NO-VERDICT {mutant.name}: its control failed")
            continue
        path.write_text(original.replace(mutant.old, mutant.new, 1), encoding="utf-8")
        try:
            code, tail = run(mutant.build)
            if code != 0:
                print(f"NO-VERDICT {mutant.name}: does not compile\n{tail[-600:]}", flush=True)
                continue
            print(f"COMPILES {mutant.name}", flush=True)
            code, tail = run(mutant.test)
            failed = [line for line in tail.splitlines() if line.startswith("test ") and line.endswith("FAILED") or "FAILED" in line and "::" in line]
            if code != 0:
                print(f"KILLED {mutant.name} ({mutant.why}): {'; '.join(failed[:3]) or tail.strip().splitlines()[-1]}", flush=True)
            else:
                print(f"SURVIVED {mutant.name} ({mutant.why})", flush=True)
        finally:
            path.write_text(original, encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
