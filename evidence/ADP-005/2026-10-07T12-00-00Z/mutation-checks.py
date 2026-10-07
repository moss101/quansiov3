#!/usr/bin/env python3
"""ADP-005 mutation checks: break each guard on purpose and show a named test then fails.

A mutant is KILLED only when all three hold: it COMPILES (the build step passes), the unmutated suite passes first
(CONTROL-PASSED for its toolchain), and a *named test* then fails. A mutant that does not compile, a control that
fails, or a failure with no named failing test is NO-VERDICT, never KILLED. A mutant that survives is SURVIVED and is a
hole in the tests.

Usage: mutation-checks.py <repo> <log> [name-filter | --check-patterns]
Run from a clean, committed tree; every mutant is restored with `git checkout -- <file>`.
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

DESK = "apps/desktop/src/main/"
LF = DESK + "local-files/"
QW = "crates/qworkerd/src/tools/"

CARGO_FAIL = re.compile(r"^test (\S+) \.\.\. FAILED", re.M)
VITEST_FAIL = re.compile(r"^\s*[×✗]\s+(.+?)(?:\s+\d+ms)?$", re.M)

VITEST_FILES = [
    "tests/local-files-config.test.ts",
    "tests/local-files-grants.test.ts",
    "tests/local-files-roots.test.ts",
    "tests/local-files-media.test.ts",
    "tests/local-files-edit.test.ts",
    "tests/local-files-watcher.test.ts",
    "tests/local-files-host.test.ts",
    "tests/local-files-links.test.ts",
    "tests/local-attach.test.ts",
    "tests/ipc-trust.test.ts",
    "tests/ipc-sources.test.ts",
    "tests/security.test.ts",
    "tests/preload.test.ts",
]
CARGO_TESTS = ["--test", "retention", "--test", "file_conflicts", "--test", "tools"]

TOOLCHAINS = {
    "rust": (
        ["cargo", "test", "-p", "quansio-qworkerd", *CARGO_TESTS],
        ["cargo", "test", "-p", "quansio-qworkerd", *CARGO_TESTS, "--no-run"],
        ".",
        CARGO_FAIL,
    ),
    "vitest": (
        ["../../node_modules/.bin/vitest", "run", *VITEST_FILES, "--reporter=verbose"],
        ["../../node_modules/.bin/tsc", "-p", "tsconfig.json"],
        "apps/desktop",
        VITEST_FAIL,
    ),
}

# (name, tool, file, old, new)
MUTANTS = [
    # ---- grants
    ("grant: a claim does not spend the grant", "vitest", LF + "grants.ts",
     "    this.#remove(grant, false);\n    const handles", "    const handles"),
    ("grant: another window may claim", "vitest", LF + "grants.ts",
     "if (grant === undefined || grant.windowId !== windowId) {", "if (grant === undefined) {"),
    ("grant: the clock is not consulted at claim", "vitest", LF + "grants.ts",
     "    if (this.#deps.now() >= grant.expiresAt) {\n      this.#remove(grant, true);\n      return \"expired\";", "    if (false) {\n      this.#remove(grant, true);\n      return \"expired\";"),
    ("grant: links are followed", "vitest", LF + "fs-ops.ts",
     "const NOFOLLOW: number = (constants as { O_NOFOLLOW?: number }).O_NOFOLLOW ?? 0;", "const NOFOLLOW: number = 0;\nvoid NOFOLLOW;"),
    ("grant: a file one byte over the limit is accepted", "vitest", LF + "grants.ts",
     "if (opened.facts.sizeBytes > limits.maxFileBytes) {", "if (opened.facts.sizeBytes > limits.maxFileBytes + 1) {"),
    ("grant: the total is not limited", "vitest", LF + "grants.ts",
     "if (total > limits.maxTotalBytes) {", "if (false) {"),
    ("grant: the live-grant count is not limited", "vitest", LF + "grants.ts",
     "if (this.#live.size >= limits.maxLive) {", "if (false) {"),
    ("grant: a closing window leaves the files it was handed open", "vitest", LF + "grants.ts",
     "    for (const handle of [...(this.#claimed.get(windowId) ?? [])]) {\n      handle.close();\n    }\n    this.#claimed.delete(windowId);", "    this.#claimed.delete(windowId);"),
    ("grant: a refused grant leaves what it opened", "vitest", LF + "grants.ts",
     "      for (const file of held) {\n        closeQuietly(file.fd);\n      }\n      return { ok: false, failure: { reason, name } };", "      return { ok: false, failure: { reason, name } };"),
    ("grant: the token is stored as it is given", "vitest", LF + "grants.ts",
     "  return createHash(\"sha256\").update(token).digest(\"hex\");", "  return token;"),
    # ---- roots and entries
    ("roots: another window's entry id resolves", "vitest", LF + "roots.ts",
     "    if (entry?.windowId !== windowId) {\n      return outside();\n    }\n    const root = this.#roots.get(entry.rootId);", "    if (entry === undefined) {\n      return outside();\n    }\n    const root = this.#roots.get(entry.rootId);"),
    ("roots: a link that leaves the root is followed", "vitest", LF + "fs-ops.ts",
     "    if (!within(rootReal, parentReal)) {\n      return { ok: false, reason: \"outside\" };\n    }", ""),
    ("roots: a link as the last element is accepted", "vitest", LF + "fs-ops.ts",
     "    if (stats.isSymbolicLink()) {\n      return { ok: false, reason: \"symlink\" };\n    }\n    if (stats.isDirectory()) {", "    if (stats.isDirectory()) {"),
    ("roots: a listing includes links", "vitest", LF + "roots.ts",
     "if (stats.isSymbolicLink() || !(stats.isFile() || stats.isDirectory())) {", "if (!(stats.isFile() || stats.isDirectory() || stats.isSymbolicLink())) {"),
    ("roots: the home folder is a root", "vitest", LF + "roots.ts",
     "if (real === \"/\" || real === realHome || /^[A-Za-z]:\\\\?$/.test(real)) {", "if (real === \"/\") {"),
    ("roots: a release keeps the entries", "vitest", LF + "roots.ts",
     "        this.#forget(id, entry);\n      }\n    }\n    this.#onReleased", "      }\n    }\n    this.#onReleased"),
    ("roots: a plain relative path may have a parent component", "vitest", LF + "fs-ops.ts",
     "return relative.split(\"/\").every((part) => part !== \"\" && part !== \".\" && part !== \"..\");", "return relative.split(\"/\").every((part) => part !== \"\" && part !== \".\");"),
    # ---- media
    ("media: another window's request is served", "vitest", LF + "media.ts",
     "(requester !== null && requester !== capability.windowId) || ", ""),
    ("media: a request nobody can name is served when the shell requires a name", "vitest", LF + "media.ts",
     "(requester === null && this.#deps.requireRequester === true)", "false"),
    ("media: any method is served", "vitest", LF + "media.ts",
     "    if (method !== \"GET\" && method !== \"HEAD\") {", "    if (false) {"),
    ("media: the sandbox policy is dropped", "vitest", LF + "media.ts",
     "  \"Content-Security-Policy\": \"sandbox\",\n", ""),
    ("media: a claimed grant's URL keeps working", "vitest", LF + "media.ts",
     "  revokeGrant(key: string): void {\n    this.#revokeWhere(", "  revokeGrant(key: string): void {\n    void key;\n    void ((): void => undefined);\n    false && this.#revokeWhere("),
    ("media: a closing window's tokens stay", "vitest", LF + "media.ts",
     "  revokeWindow(windowId: number): void {\n    this.#revokeWhere((capability) => capability.windowId === windowId);", "  revokeWindow(windowId: number): void {\n    void windowId;"),
    ("media: a suffix range starts one byte late", "vitest", LF + "media.ts",
     "return { kind: \"partial\", start: Math.max(0, size - count), end: size - 1 };", "return { kind: \"partial\", start: Math.max(0, size - count + 1), end: size - 1 };"),
    ("media: more than one range is served", "vitest", LF + "media.ts",
     "    return /^\\s*bytes\\s*=/i.test(header) ? { kind: \"unsatisfiable\" } : { kind: \"none\" };", "    return { kind: \"none\" };"),
    ("media: HTML is served as itself", "vitest", LF + "results.ts",
     "  return served.test(type) ? type : \"application/octet-stream\";", "  return type;"),
    ("media: the page's policy replaces a media response's", "vitest", DESK + "security.ts",
     "details.url.startsWith(\"quansio-media:\") ? MEDIA_RESPONSE_CSP : RENDERER_CSP", "RENDERER_CSP"),
    ("media: the page may load the scheme as a script", "vitest", DESK + "security.ts",
     "default-src 'self'; script-src 'self';", "default-src 'self' quansio-media:; script-src 'self';"),
    ("media: the per-window cap is not applied", "vitest", LF + "media.ts",
     "    for (const [key] of own.slice(0, Math.max(0, own.length - this.#deps.maxTokens))) {", "    for (const [key] of own.slice(0, 0)) {"),
    # ---- safe edits
    ("edit: the re-check after the temporary file is skipped", "vitest", LF + "safe-edit.ts",
     "  const second = statNow(target);\n  if (!unchanged(second, effective)) {", "  const second = statNow(target);\n  if (false) {"),
    ("edit: the baseline is not compared before the write", "vitest", LF + "safe-edit.ts",
     "  if (!unchanged(first, effective)) {\n    if (onConflict", "  if (first === null) {\n    if (onConflict"),
    ("edit: an existing backup is replaced", "vitest", LF + "safe-edit.ts",
     "copyFileSync(target, candidate, constants.COPYFILE_EXCL);", "copyFileSync(target, candidate);"),
    ("edit: the file's permissions are not kept", "vitest", LF + "safe-edit.ts",
     "const mode = effective.mode & 0o7777;", "const mode = 0o600;"),
    ("edit: the temporary file is left behind on a conflict", "vitest", LF + "safe-edit.ts",
     "  if (!unchanged(second, effective)) {\n    removeQuietly(temporary);", "  if (!unchanged(second, effective)) {"),
    ("edit: a write is not recorded for the watcher", "vitest", LF + "safe-edit.ts",
     "    this.#deps.onWritten?.(resolved.result.absolute, outcome.saved.baseline);", ""),
    ("edit: the baseline stays the old one after a save", "vitest", LF + "safe-edit.ts",
     "    session.baseline = outcome.saved.baseline;", ""),
    ("edit: a target that became a link is written through", "vitest", LF + "safe-edit.ts",
     "  if (first !== null && first.isSymbolicLink()) {\n    return { ok: false, kind: \"refused\", reason: \"symlink\" };\n  }", ""),
    ("edit: binary content is opened as text", "vitest", LF + "safe-edit.ts",
     "    if (bytes.includes(0)) {\n      return { ok: false, reason: \"not_text\" };\n    }\n    let text", "    let text"),
    # ---- watcher
    ("watcher: one OS watch per path, not per inode", "vitest", LF + "watcher.ts",
     "key = `${stats.dev.toString()}:${stats.ino.toString()}`;\n    } catch {\n      return refusedFile(\"unreadable\"", "key = directory;\n    } catch {\n      return refusedFile(\"unreadable\""),
    ("watcher: no debounce", "vitest", LF + "watcher.ts",
     "}, Math.min(limits.debounceMs, ceiling));", "}, 0);"),
    ("watcher: a replaced directory is still watched", "vitest", LF + "watcher.ts",
     "if (!stats.isDirectory() || `${stats.dev.toString()}:${stats.ino.toString()}` !== dir.key) {", "if (!stats.isDirectory()) {"),
    ("watcher: the queue limit is one too high", "vitest", LF + "watcher.ts",
     "if (reported.length > this.#deps.limits.queueMax) {", "if (reported.length >= this.#deps.limits.queueMax) {"),
    ("watcher: a shell write is reported back", "vitest", LF + "watcher.ts",
     "return !(own !== undefined && own.expiresAt > now && seen !== undefined && own.sizeBytes === seen.sizeBytes && own.mtimeNs === seen.mtimeNs);", "return true;"),
    ("watcher: subscribers are not limited", "vitest", LF + "watcher.ts",
     "if (this.subscriberCount(windowId) >= limits.maxSubscribers) {", "if (false) {"),
    ("watcher: watches are not limited", "vitest", LF + "watcher.ts",
     "if (this.#dirs.size >= limits.maxDirs) {", "if (false) {"),
    ("watcher: a closed window's watch stays open", "vitest", LF + "watcher.ts",
     "  revokeWindow(windowId: number): void {\n    this.#revokeWhere((subscription) => subscription.windowId === windowId);", "  revokeWindow(windowId: number): void {\n    void windowId;"),
    ("watcher: an entry of a file is accepted as a folder", "vitest", LF + "watcher.ts",
     "if (!stats.isDirectory()) {\n        return refusedFile(\"not_regular\", \"only a folder can be watched\");", "if (false) {\n        return refusedFile(\"not_regular\", \"only a folder can be watched\");"),
    # ---- host, policy, links
    ("host: the switch does not switch anything off", "vitest", LF + "host.ts",
     "        if (!o.config.enabled) {\n          return Promise.resolve(refuse(\"POLICY_DENIED\", \"local files are switched off\"));\n        }\n        const parsed", "        const parsed"),
    ("host: a second dialog may open in one window", "vitest", LF + "host.ts",
     "    if (this.#dialogs.has(id)) {\n      this.#o.log(", "    if (false) {\n      this.#o.log("),
    ("host: a closing window keeps its grants", "vitest", LF + "host.ts",
     "    this.grants.revokeWindow(id);\n    this.roots.revokeWindow(id);", "    this.roots.revokeWindow(id);"),
    ("host: a closing window keeps its watches", "vitest", LF + "host.ts",
     "    this.watcher.revokeWindow(id);\n    this.#dialogs.delete(id);", "    this.#dialogs.delete(id);"),
    ("host: the attach leaves the claimed files open", "vitest", LF + "host.ts",
     "      for (const file of claim.files) {\n        file.close();\n      }\n    }\n    return done({ attached, skipped });", "    }\n    return done({ attached, skipped });"),
    ("host: a file inside Quansio's data directory can be attached", "vitest", LF + "host.ts",
     "    const forbidden = this.#protectedFile(picked);\n    if (forbidden !== null) {", "    const forbidden = null as string | null;\n    if (forbidden !== null) {"),
    ("host: a folder holding Quansio's data directory can be opened", "vitest", LF + "host.ts",
     "return real !== null && this.#protectedReal().some((guarded) => inside(real, guarded) || inside(guarded, real));", "return real !== null && this.#protectedReal().some((guarded) => inside(real, guarded));"),
    ("host: an oversized file is sent to the upload", "vitest", LF + "host.ts",
     "} else if (file.sizeBytes > this.#o.maxUploadBytes) {", "} else if (false) {"),
    ("host: a payload may carry a field the channel does not name", "vitest", DESK + "schema.ts",
     "        if (!known.has(key)) {\n          throw new SchemaError(`${path}.${key}`, \"is not a known field\");\n        }", ""),
    ("policy: a pop-out reaches the folder picker", "vitest", DESK + "ipc-trust.ts",
     "\"files.root.pick\": { windows: [\"main\"] },", "\"files.root.pick\": { windows: [\"main\", \"popout\"], popout: () => null },"),
    ("policy: a pop-out asks for a dialog for any thread", "vitest", DESK + "ipc-trust.ts",
     "\"files.pick\": { windows: [\"main\", \"popout\"], popout: own(\"threadId\") },", "\"files.pick\": { windows: [\"main\", \"popout\"], popout: () => null },"),
    ("policy: quick entry reaches the grant channels", "vitest", DESK + "ipc-trust.ts",
     "\"files.grant.attach\": { windows: [\"main\", \"popout\"], popout: own(\"threadId\") },", "\"files.grant.attach\": { windows: [\"main\", \"popout\", \"quick_entry\"], popout: own(\"threadId\") },"),
    ("link: an attach link may carry a query", "vitest", DESK + "deeplinks.ts",
     "  if (url.search) {\n    // Any query", "  if (url.search && url.hostname !== \"attach\") {\n    // Any query"),
    ("link: an attach link may carry a sub-path", "vitest", DESK + "deeplinks.ts",
     "    if (parts.length === 1 && only.startsWith(\"thr_\") && ULID_BODY.test(only.slice(4))) {", "    if (parts.length >= 1) {"),
    # ---- worktree retention and agent writes (Rust)
    ("retention: an edited prepared file is accepted", "rust", QW + "scm/retention.rs",
     "if set.token != token || token_of(&set.entries) != token {", "if false {"),
    ("retention: any set of ids is accepted for a token", "rust", QW + "scm/retention.rs",
     "        if set.ids() != requested {", "        if false {"),
    ("retention: an expired token is honoured", "rust", QW + "scm/retention.rs",
     "        if self.now() > set.expires_at_ms {", "        if false {"),
    ("retention: the preview's index and status are not compared", "rust", QW + "scm/retention.rs",
     "        if live.fingerprint != entry.fingerprint {\n            return Ok(Self::skip(\n                SkipReason::IndexChanged,\n                \"the index or the working tree changed after the preview\",",
     "        if false {\n            return Ok(Self::skip(\n                SkipReason::IndexChanged,\n                \"the index or the working tree changed after the preview\","),
    ("retention: the content of a changed file is not part of the fingerprint", "rust", QW + "scm/retention.rs",
     "            material.extend_from_slice(content_marker(&path.join(&relative)).as_bytes());", "            material.extend_from_slice(b\"-\");"),
    ("retention: the last look before removal is skipped", "rust", QW + "scm/retention.rs",
     "        if last.fingerprint != entry.fingerprint {", "        if false {"),
    ("retention: HEAD moving is not noticed", "rust", QW + "scm/retention.rs",
     "        if live.head != entry.head || live.head != record.head {", "        if false {"),
    ("retention: a held worktree is removed", "rust", QW + "scm/retention.rs",
     "            WorktreeState::Active => return Ok(Self::skip(SkipReason::Active, \"a Run holds it\")),\n            WorktreeState::Idle => {}\n        }\n        if record.activity.holders > 0 {",
     "            WorktreeState::Active | WorktreeState::Idle => {}\n        }\n        if false {"),
    ("retention: the snapshot is built in the worktree's real index", "rust", QW + "scm/retention.rs",
     "                let env = [(\"GIT_INDEX_FILE\", scratch_text.as_str())];\n                self.git(path, &[\"read-tree\", &head], &env)?;", "                let env: [(&str, &str); 0] = [];\n                let _ = &scratch_text;\n                self.git(path, &[\"read-tree\", &head], &env)?;"),
    ("retention: the index snapshot is written with write-tree on the live index", "rust", QW + "scm/retention.rs",
     "                self.git(path, &[\"write-tree\"], &staged_env)\n            })();", "                self.git(path, &[\"write-tree\"], &[])\n            })();"),
    ("retention: removal is not recorded before it begins", "rust", QW + "scm/retention.rs",
     "        record.state = WorktreeState::Removing;\n        record.removal = Some(Removal {", "        record.removal = Some(Removal {"),
    ("retention: a crash recovery completes a changed worktree", "rust", QW + "scm/retention.rs",
     "                    (Some(removal), Ok(live)) => live.fingerprint == removal.fingerprint,", "                    (Some(_), Ok(_)) => true,"),
    ("retention: a crash recovery abandons a complete removal", "rust", QW + "scm/retention.rs",
     "            if !path.exists() {\n                let _ = self.git(Path::new(&record.repo), &[\"worktree\", \"prune\"], &[]);\n                self.finish_removal(&mut record)?;\n                report.completed.push(id);\n                continue;\n            }", "            if !path.exists() {\n                continue;\n            }"),
    ("retention: the sweep removes a worktree with changes", "rust", QW + "scm/retention.rs",
     "                    if !live.dirty && !live.unmerged {", "                    if !live.unmerged {"),
    ("retention: the sweep ignores how long a worktree was idle", "rust", QW + "scm/retention.rs",
     "                        && now.saturating_sub(record.activity.last_activity_at_ms)\n                            >= self.config.sweep_min_idle_ms",
     "                        && now >= record.activity.last_activity_at_ms"),
    ("retention: the sweep takes the newest first", "rust", QW + "scm/retention.rs",
     "                    .cmp(&b.activity.last_activity_at_ms)", "                    .cmp(&b.activity.last_activity_at_ms)\n                    .reverse()"),
    ("retention: a recovery ref outside the private namespace is accepted", "rust", QW + "scm/retention.rs",
     "    let Some(rest) = value.strip_prefix(\"refs/quansio/\") else {\n        return false;\n    };", "    let Some(rest) = value.strip_prefix(\"refs/\") else {\n        return false;\n    };"),
    ("retention: a restore leaves the index as the snapshot's tree", "rust", QW + "scm/retention.rs",
     "            self.git(&path, &[\"read-tree\", &format!(\"{index}^{{tree}}\")], &[])?;",
     "            let _ = &index;"),
    ("retention: an id with a path in it is accepted", "rust", QW + "scm/retention.rs",
     "        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')\n}\n\n// ---", "        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' || c == '/' || c == '.')\n}\n\n// ---"),
    ("agent write: the re-check after the temporary file is skipped", "rust", QW + "files.rs",
     "        if let Some(expected) = expected {\n            let current = match", "        if let Some(expected) = expected.filter(|_| false) {\n            let current = match"),
    ("agent write: a stale base digest is accepted", "rust", QW + "files.rs",
     "            if current != base {\n                return Err(ToolError::EditConflict {\n                    path: path.relative().to_string(),\n                    expected_digest: base.to_string(),\n                    actual_digest: current,\n                });\n            }\n        }\n        self.replace_atomically(&path, content, base_digest)?;", "        }\n        self.replace_atomically(&path, content, None)?;"),
    ("agent write: a patch is applied to bytes other than the ones it was checked against", "rust", QW + "files.rs",
     "self.replace_atomically(&path, applied.content.as_bytes(), Some(&read_digest))?;", "self.replace_atomically(&path, applied.content.as_bytes(), None)?;"),
    ("agent write: the temporary file is not created exclusively", "rust", QW + "files.rs",
     "                .create_new(true)\n                .open(&temporary)?;", "                .create(true)\n                .truncate(true)\n                .open(&temporary)?;"),
    ("agent write: the file's permissions are not kept", "rust", QW + "files.rs",
     "                file.set_permissions(metadata.permissions())?;", "                let _ = &metadata;"),
]


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=REPO, check=True, capture_output=True, text=True).stdout


def run(cmd: list[str], cwd: str) -> tuple[int, str]:
    result = subprocess.run(cmd, cwd=REPO / cwd, env=ENV, capture_output=True, text=True)
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
            count = (REPO / file).read_text().count(old)
            if count != 1:
                bad += 1
                say(f"PATTERN {count} {name}")
        say(f"patterns checked: {len(MUTANTS)}, bad: {bad}")
        return
    if git("status", "--porcelain", "--untracked-files=no").strip():
        sys.exit("the tree must be clean and committed")
    say(f"# ADP-005 mutation checks at {git('rev-parse', '--short', 'HEAD').strip()}")
    controls: dict[str, bool] = {}
    killed = survived = nov = 0
    for name, tool, file, old, new in MUTANTS:
        if FILTER and FILTER not in name:
            continue
        test_cmd, build_cmd, cwd, failing = TOOLCHAINS[tool]
        if tool not in controls:
            run(build_cmd, cwd)
            code, text = run(test_cmd, cwd)
            controls[tool] = code == 0
            say(f"CONTROL-{'PASSED' if code == 0 else 'FAILED'} {tool}")
            if code != 0:
                say(text[-3000:])
        path = REPO / file
        original = path.read_text()
        if original.count(old) != 1:
            say(f"NO-VERDICT (pattern found {original.count(old)} times) {name}")
            nov += 1
            continue
        try:
            path.write_text(original.replace(old, new, 1))
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
            subprocess.run(["git", "checkout", "--", file], cwd=REPO, check=True)
    say(f"# mutants: {killed + survived + nov}  killed: {killed}  survived: {survived}  no-verdict: {nov}")


main()
