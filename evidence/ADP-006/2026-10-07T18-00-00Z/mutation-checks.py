#!/usr/bin/env python3
"""ADP-006 mutation checks: break each guard on purpose and show a named test then fails.

A mutant is KILLED only when all three hold: it COMPILES (the build step passes), the unmutated suite passes first
(CONTROL-PASSED for its toolchain), and a *named test* then fails. A mutant that does not compile, a control that
fails, or a failure with no named failing test is NO-VERDICT, never KILLED. A mutant that survives is SURVIVED and is a
hole in the tests.

Usage: mutation-checks.py <repo> <log> [name-filter | --check-patterns]
Run from a clean, committed tree; every mutant is restored with `git checkout -- <file>`.
The Rust suites need the private test database and the development object store in the environment
(QUANSIO_TEST_POSTGRES_URL, QUANSIO_TEST_MINIO_*), and Google Chrome for the deck and document suites.
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

OUT = "crates/server/src/artifacts/outputs/"
PV = OUT + "preview/"
QW = "crates/qworkerd/src/"
PY = "python/intelligence/"
DESK = "apps/desktop/src/"

CARGO_FAIL = re.compile(r"^test (\S+) \.\.\. FAILED", re.M)
PYTEST_FAIL = re.compile(r"^FAILED (\S+)", re.M)
VITEST_FAIL = re.compile(r"^\s*[×✗]\s+(.+?)(?:\s+\d+ms)?$", re.M)


def cargo(package: str, *selectors: str) -> tuple[list[str], list[str], str, re.Pattern[str]]:
    return (
        ["cargo", "test", "-p", package, *selectors],
        ["cargo", "test", "-p", package, *selectors, "--no-run"],
        ".",
        CARGO_FAIL,
    )


PY_TESTS = [
    "tests/intelligence/test_deck_outputs.py",
    "tests/intelligence/test_document_diagram_outputs.py",
    "tests/intelligence/test_video_outputs.py",
    "tests/intelligence/test_transcription_outputs.py",
]
# A positional test name filters every selected target, so a unit suite and an integration suite are separate
# toolchains: `lib` runs the unit tests under `outputs::`, the others run one whole integration binary.
TOOLCHAINS = {
    "lib": cargo("quansio-server", "--lib", "outputs::"),
    "deck": cargo("quansio-server", "--test", "outputs_deck"),
    "docs": cargo("quansio-server", "--test", "outputs_documents"),
    "video": cargo("quansio-server", "--test", "outputs_video"),
    "stt": cargo("quansio-server", "--test", "outputs_transcribe"),
    "preview": cargo("quansio-server", "--test", "outputs_preview"),
    "compose": cargo("quansio-server", "--test", "outputs_compose"),
    "qworkerd-lib": cargo("quansio-qworkerd", "--lib", "media::"),
    "qworkerd-media": cargo("quansio-qworkerd", "--test", "media"),
    "qworkerd-render": cargo("quansio-qworkerd", "--test", "render"),
    "qworkerd-render-lib": cargo("quansio-qworkerd", "--lib", "browser::"),
    "python": (
        ["uv", "run", "pytest", "-q", "-x", "-p", "no:cacheprovider", *PY_TESTS],
        ["uv", "run", "python", "-c", "import intelligence.artifacts.outputs.decks, intelligence.artifacts.outputs.markdown, intelligence.artifacts.outputs.diagrams, intelligence.artifacts.outputs.pptx, intelligence.artifacts.outputs.transcripts, intelligence.model_gateway.transcription"],
        "python",
        PYTEST_FAIL,
    ),
    "vitest": (
        ["../../node_modules/.bin/vitest", "run", "tests/outputs.test.ts", "tests/artifacts.test.ts", "--reporter=verbose"],
        ["../../node_modules/.bin/tsc", "-p", "tsconfig.json"],
        "apps/desktop",
        VITEST_FAIL,
    ),
}

# (name, toolchain, file, old, new)  -- old/new may be lists when a guard has a second layer that must go too.
# Not listed on purpose: `all_ready` in deck.render's answer (`all_ready && gate.deliverable`) is a second layer over the
# gate, which recomputes every slide's state from the stored source; removing the first layer changes no behaviour.
MUTANTS = [
    # ---- the quality record
    ("quality: a record that checked nothing is ok", "lib", OUT + "quality.rs",
     "!self.checks.is_empty() && self.checks.iter().all(|check| check.ok)", "self.checks.iter().all(|check| check.ok)"),
    ("quality: one passing check makes a record ok", "lib", OUT + "quality.rs",
     "!self.checks.is_empty() && self.checks.iter().all(|check| check.ok)", "!self.checks.is_empty() && self.checks.iter().any(|check| check.ok)"),
    # ---- the deck gate
    ("deck gate: a changed slide stays what it was", "lib", OUT + "deck.rs",
     "Some(entry) if entry.source_digest != *digest => SlideStanding {", "Some(entry) if false && entry.source_digest != *digest => SlideStanding {"),
    ("deck gate: pages for another count of slides deliver", "lib", OUT + "deck.rs",
     "        && usable.is_some_and(|pages| pages.deck.slide_count == deck.slides.len());", "        && usable.is_some();"),
    ("deck gate: one ready page delivers the deck", "lib", OUT + "deck.rs",
     "&& slides.iter().all(|slide| slide.state == SlideState::Ready)", "&& slides.iter().any(|slide| slide.state == SlideState::Ready)"),
    ("deck gate: pages from another template version are used", "lib", OUT + "deck.rs",
     "let usable = pages.filter(|pages| pages.template_version == template_version);", "let usable = pages;"),
    ("deck pages: a ready page with a failing check is accepted", "lib", OUT + "deck.rs",
     "if entry.state == SlideState::Ready && entry.checks.iter().any(|check| !check.ok) {", "if false {"),
    ("deck pages: a failed page with no failing check is accepted", "lib", OUT + "deck.rs",
     "if entry.state == SlideState::FailedQuality && entry.checks.iter().all(|check| check.ok)", "if false && entry.checks.iter().all(|check| check.ok)"),
    ("deck host: pages an upload made are the authority's own", "deck", OUT + "deck_host.rs",
     "    if !by_the_authority {", "    if false && !by_the_authority {"),
    ("deck host: a carried page need not match its slide", "deck", OUT + "deck_host.rs",
     "entry.source_digest == *digest\n                        && matches!(entry.state, SlideState::Ready | SlideState::FailedQuality)",
     "matches!(entry.state, SlideState::Ready | SlideState::FailedQuality)"),
    # ---- sources and tool-owned roles
    ("support: an artifact of another workspace is read", "stt", OUT + "support.rs",
     "    if artifact.workspace_id != ctx.dispatch.workspace_id {\n        return Err(refuse(\n            \"SCOPE_FORBIDDEN\",\n            \"artifact.workspace\",\n            format!(\"{artifact_id} is not an artifact of this run's workspace\"),\n        ));\n    }\n    if artifact.kind != kind {",
     "    if artifact.kind != kind {"),
    ("support: another kind of artifact is read", "stt", OUT + "support.rs",
     "    if artifact.kind != kind {", "    if false && artifact.kind != kind {"),
    ("tool owned: artifact.create may use a reserved role", "deck", OUT + "tool_owned.rs",
     "    if reserved_role(role) {\n        return Some(format!(\n            \"the role {role:?} belongs to the output tools; they make that artifact, `artifact.create` cannot\"", "    if false && reserved_role(role) {\n        return Some(format!(\n            \"the role {role:?} belongs to the output tools; they make that artifact, `artifact.create` cannot\""),
    ("tool owned: artifact.create may store deck pages", "deck", OUT + "tool_owned.rs",
     "    if MEDIA_TYPES.contains(&media_type) {", "    if false && MEDIA_TYPES.contains(&media_type) {"),
    ("tool owned: artifact.update may change a timeline", "video", OUT + "tool_owned.rs",
     "    if current_media_type == TIMELINE_MEDIA_TYPE || new_media_type == TIMELINE_MEDIA_TYPE {", "    if false {"),
    # ---- printed documents
    ("pdf: a file that is not a PDF is accepted", "lib", OUT + "pdf.rs",
     "    if !bytes.starts_with(b\"%PDF-1.\") {", "    if false && !bytes.starts_with(b\"%PDF-1.\") {"),
    ("pdf: a cut-short file is accepted", "lib", OUT + "pdf.rs",
     "    if !contains(tail, b\"%%EOF\") {", "    if false && !contains(tail, b\"%%EOF\") {"),
    # ---- video
    ("video edit: a stale revision is applied", "video", OUT + "video_host.rs",
     "    if expected != seq {\n        // The one stale call", "    if false && expected != seq {\n        // The one stale call"),
    ("video edit: a dry run commits", "video", OUT + "video_host.rs",
     "    if dry_run {\n        return Ok(success(", "    if false && dry_run {\n        return Ok(success("),
    ("video edit: a medium of the wrong kind is imported", "video", OUT + "video_host.rs",
     "        if artifact.kind != wanted {", "        if false && artifact.kind != wanted {"),
    ("video edit: a version that is not its own revision is edited", "video", OUT + "video_host.rs",
     "    if current.revision != seq {\n        return Err(refuse(\n            \"CONFLICT_STATE\",\n            \"output.video.revision\",\n            format!(\n                \"version {seq} of this timeline says it is revision {}: it was not made by video.edit\"",
     "    if false && current.revision != seq {\n        return Err(refuse(\n            \"CONFLICT_STATE\",\n            \"output.video.revision\",\n            format!(\n                \"version {seq} of this timeline says it is revision {}: it was not made by video.edit\""),
    ("video edit: a batch is not bounded", "video", OUT + "video_host.rs",
     ".is_some_and(|ops| ops.len() > limits.max_ops_per_batch)", ".is_some_and(|ops| ops.len() > limits.max_ops_per_batch * 1000)"),
    ("video ops: a later operation is not checked against the earlier ones", "video", OUT + "video/ops.rs",
     "        work.validate(limits).map_err(|detail| OpError {\n            index,\n            rule: RULE_TIMELINE,\n            detail,\n            field: false,\n        })?;\n", ""),
    # ---- speech to text
    ("stt: a transcript of other audio is accepted", "stt", OUT + "transcribe_host.rs",
     "let from_this_audio = found.source == source;", "let from_this_audio = true;"),
    ("stt: the same id for another language is the same job", "stt", OUT + "transcribe_host.rs",
     "|| !same_language ||", "|| false ||"),
    ("stt: the same id for other audio is the same job", "stt", OUT + "transcribe_host.rs",
     "if !same_source ||", "if false ||"),
    ("stt: audio over the limit is sent", "stt", OUT + "transcribe_host.rs",
     "    if audio.bytes.len() > limits.max_audio_bytes {", "    if false && audio.bytes.len() > limits.max_audio_bytes {"),
    ("stt: any request id is accepted", "stt", OUT + "transcribe_host.rs",
     "    if !valid_request_id(&request_id) {", "    if false && !valid_request_id(&request_id) {"),
    ("stt: an unknown error code is passed to the model", "stt", OUT + "transcribe_host.rs",
     "let code = if PASSED_CODES.contains(&code.as_str()) {", "let code = if true {"),
    ("stt: an answer is stored without the transcript rules", "stt", OUT + "transcribe_host.rs",
     "let parsed = Transcript::parse(&bytes, limits);", "let parsed = serde_json::from_slice::<Transcript>(&bytes).map_err(|e| e.to_string());"),
    ("transcript: the untrusted label may be dropped", "lib", OUT + "transcript.rs",
     "        if self.trust != TRUST {", "        if false && self.trust != TRUST {"),
    ("transcript: segments may overlap", "lib", OUT + "transcript.rs",
     "                if segment.start_ms + limits.segment_overlap_ms < end {", "                if false && segment.start_ms + limits.segment_overlap_ms < end {"),
    # ---- the preview
    ("composition: the preview server is not mounted in the deployable", "compose", "crates/server/src/composition.rs",
     "crates::artifacts::outputs::OutputToolHost::new(pool.clone(), objects)\n                    .with_preview(Arc::new(preview)),".replace("crates::", "crate::"),
     "crate::artifacts::outputs::OutputToolHost::new(pool.clone(), objects),"),
    ("preview: a scope does not expire", "preview", PV + "service.rs",
     "        if self.clock.now_ms() >= scope.expires_ms {", "        if false && self.clock.now_ms() >= scope.expires_ms {"),
    ("preview: the authority is not asked again", "preview", PV + "service.rs",
     "        match self.authority.resolve(&scope.binding).await {",
     "        match Ok::<Resolved, AuthorityError>(Resolved {\n            workspace_root: std::path::PathBuf::new(),\n            generation: scope.generation.clone(),\n        }) {"),
    ("preview: a changed root keeps its scope", "preview", PV + "service.rs",
     "            Ok(resolved) if resolved.generation == scope.generation => Ok(Active {", "            Ok(resolved) => Ok(Active {"),
    ("preview: a lapse is forgotten", "preview", PV + "service.rs",
     "            registry.remember(token.to_string(), tomb.clone());", "            let _ = &tomb;"),
    ("preview: a renewal passes the maximum lifetime", "preview", PV + "service.rs",
     "let expires = (now + self.limits.ttl_ms).min(ceiling);", "let expires = now + self.limits.ttl_ms;"),
    ("preview: another member may renew, close or read a scope", "preview", PV + "service.rs",
     "            && binding_principal(binding) == principal_of(dispatch)", ""),
    ("preview: the scope limit is not applied", "preview", PV + "service.rs",
     "        if registry.live.len() >= self.limits.max_scopes {", "        if false && registry.live.len() >= self.limits.max_scopes {"),
    ("preview: a directory may climb out of the root", "preview", PV + "service.rs",
     "            \"..\" => return Err(\"directory does not leave the workspace root\".to_string()),", "            \"..\" => {}"),
    ("preview: a hidden directory may be opened", "preview", PV + "service.rs",
     "            other if other.starts_with('.') => {", "            other if false && other.starts_with('.') => {"),
    ("preview: a directory behind a link out of the root is opened", "preview", PV + "service.rs",
     "        if !base.starts_with(&resolved.workspace_root) || !is_directory {", "        if !is_directory {"),
    ("preview authority: a removed member is served", "preview", PV + "authority.rs",
     "            if member.as_deref() != Some(\"active\") {", "            if false && member.as_deref() != Some(\"active\") {"),
    ("preview authority: a run that ended is served", "preview", PV + "authority.rs",
     "        if TERMINAL_RUN.contains(&status.as_str()) {", "        if false && TERMINAL_RUN.contains(&status.as_str()) {"),
    ("preview authority: a destroyed target is served", "preview", PV + "authority.rs",
     "        if GONE_TARGET.contains(&target_status.as_str()) || target_workspace != binding.workspace_id\n        {", "        if false {"),
    ("preview serve: a dotfile path is served", "lib", PV + "serve.rs",
     ".any(|segment| segment.is_empty() || segment.starts_with('.'))", ".any(|segment| segment.is_empty())"),
    ("preview serve: an escaped separator is allowed", "lib", PV + "serve.rs",
     "    if decoded.contains('\\0') || decoded.contains('\\\\') || decoded.chars().any(char::is_control) {", "    if decoded.contains('\\0') {"),
    ("preview serve: a link that leaves the directory is followed", "preview", PV + "serve.rs",
     ["    let Ok(relative) = resolved.strip_prefix(&base) else {\n        // A link that leaves the directory is not served, and is not distinguished from a miss.\n        return Err(FileError::NotFound);\n    };", "        _ => true,\n    }) {"],
     ["    let relative = resolved.strip_prefix(&base).unwrap_or(&resolved);", "        _ => false,\n    }) {"]),
    ("preview serve: a link to a dotfile is served", "preview", PV + "serve.rs",
     "        Component::Normal(name) => name.to_string_lossy().starts_with('.'),", "        Component::Normal(_) => false,"),
    ("preview serve: a request for another host is answered", "preview", PV + "serve.rs",
     "    host == format!(\"127.0.0.1:{port}\") || host == format!(\"localhost:{port}\")", "    let _ = port;\n    !host.is_empty()"),
    ("preview serve: the sandbox policy is dropped", "preview", PV + "serve.rs",
     "        headers.insert(header::CONTENT_SECURITY_POLICY, policy);", "        let _ = policy;"),
    ("preview serve: sniffing is allowed", "preview", PV + "serve.rs",
     "    headers.insert(\n        header::X_CONTENT_TYPE_OPTIONS,\n        HeaderValue::from_static(\"nosniff\"),\n    );\n", ""),
    ("preview serve: a file over the limit is served", "preview", PV + "serve.rs",
     "    if meta.len() > max_bytes {", "    if false && meta.len() > max_bytes {"),
    ("preview suggestions: a suggestion that says nothing is stored", "preview", PV + "suggest.rs",
     "        if self.before.is_none() && self.after.is_none() && self.note.is_none() {", "        if false {"),
    ("preview suggestions: an oversized field is stored", "preview", PV + "suggest.rs",
     "                if !plain(value, 2048) {", "                if false && !plain(value, 2048) {"),
    ("preview suggestions: a scope takes any number", "preview", PV + "service.rs",
     "        if log.suggestions.len() >= self.limits.max_suggestions {", "        if false && log.suggestions.len() >= self.limits.max_suggestions {"),
    ("preview suggestions: two at once may overwrite each other", "preview", PV + "service.rs",
     "        let _one_at_a_time = active.lock.lock().await;\n", ""),
    # ---- the worker: sandbox and supervision
    ("render: a request outside the pinned assets is answered", "qworkerd-render-lib", QW + "browser/render.rs",
     "    if host != ASSET_HOST {\n        return None;\n    }", ""),
    ("render: script is not disabled", "qworkerd-render", QW + "browser/render.rs",
     "        \"Emulation.setScriptExecutionDisabled\",\n        json!({ \"value\": true }),", "        \"Emulation.setScriptExecutionDisabled\",\n        json!({ \"value\": false }),"),
    ("media: a path in a file name is accepted", "qworkerd-lib", QW + "media.rs",
     "        && !name.starts_with('.')\n", ""),
    ("media: an id that is not a digest is accepted", "qworkerd-lib", QW + "media.rs",
     "    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())", "    !id.is_empty()"),
    ("media: a job outruns its timeout", "qworkerd-media", QW + "media.rs",
     "        let waited = tokio::time::timeout(timeout, child.wait()).await;", "        let waited = tokio::time::timeout(timeout * 1000, child.wait()).await;"),
    ("media: a cancelled job's children survive", "qworkerd-media", QW + "media.rs",
     "        .args([\"-KILL\", \"--\", &format!(\"-{pid}\")])", "        .args([\"-KILL\", \"--\", &format!(\"{pid}\")])"),
    # ---- the intelligence plane
    ("python: a slide's digest depends on key order", "python", PY + "artifacts/outputs/decks.py",
     "json.dumps(slide, sort_keys=True, separators=(\",\", \":\"), ensure_ascii=False)", "json.dumps(slide, sort_keys=False, separators=(\",\", \":\"), ensure_ascii=False)"),
    ("python: Markdown text is not escaped", "python", PY + "artifacts/outputs/markdown.py",
     "    text = _emphasis(escape(text))\n    return re.sub", "    text = _emphasis(text)\n    return re.sub"),
    ("python: a remote image is fetched when printed", "python", PY + "artifacts/outputs/markdown.py",
     "        if _INLINE_IMAGE.fullmatch(target):", "        if True:"),
    ("python: a diagram title is not escaped", "python", PY + "artifacts/outputs/diagrams.py",
     "    title = escape(diagram.title)", "    title = diagram.title"),
    ("python: a package with a bad checksum is accepted", "python", PY + "artifacts/outputs/pptx.py",
     "    if bad_crc:\n        return [*problems, f\"{bad_crc}: CRC mismatch\"]", "    if False:\n        return [*problems, f\"{bad_crc}: CRC mismatch\"]"),
    ("python: audio over the limit is sent to a provider", "python", PY + "model_gateway/transcription.py",
     "    if len(audio) > max_bytes:", "    if False:"),
    ("python: the data policy is not checked before a transcription is built", "python", PY + "model_gateway/gateway.py",
     "            self._dlp_decision = self._dlp.check(request, model, provider)\n        base_url = provider.resolve_base_url(self._environ)\n        credential = self._credentials.resolve(provider)\n        wire = wire_for_kind(provider.kind)",
     "            pass\n        base_url = provider.resolve_base_url(self._environ)\n        credential = self._credentials.resolve(provider)\n        wire = wire_for_kind(provider.kind)"),
    ("python: a request id for other audio starts a second job", "python", PY + "artifacts/outputs/transcripts.py",
     "            if job is not None and job.fingerprint != fingerprint:", "            if False:"),
    ("python: a transcript segment may run past the recording", "python", PY + "artifacts/outputs/transcripts.py",
     "        if segment.start_ms >= segment.end_ms or segment.end_ms > provider.duration_ms:", "        if False:"),
    # ---- the browser
    ("desktop: a record that lists a failing check is ok", "vitest", DESK + "artifacts/outputs.ts",
     "    ok: value[\"ok\"] && checks.length > 0 && checks.every((check) => check.ok),", "    ok: value[\"ok\"],"),
    ("desktop: an unknown page state counts as ready", "vitest", DESK + "artifacts/outputs.ts",
     "PAGE_STATES.find((candidate) => candidate === raw) ?? \"pending\"", "PAGE_STATES.find((candidate) => candidate === raw) ?? \"ready\""),
    ("desktop: outside content is not labelled", "vitest", DESK + "artifacts/outputs.ts",
     "  return quality?.trust === \"UNTRUSTED_EXTERNAL\";", "  return false;"),
    ("desktop: structured output is not shown as text", "vitest", DESK + "artifacts/preview.ts",
     "kind.endsWith(\"+json\") || ", ""),
]


def as_list(value):
    """A mutant's `old`/`new` is one string, or a list of strings when a guard has a second layer that must go too."""
    return value if isinstance(value, list) else [value]


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
            count = min((REPO / file).read_text().count(part) for part in as_list(old))
            if count != 1:
                bad += 1
                say(f"PATTERN {count} {name}")
        say(f"patterns checked: {len(MUTANTS)}, bad: {bad}")
        return
    if git("status", "--porcelain", "--untracked-files=no").strip():
        sys.exit("the tree must be clean and committed")
    say(f"# ADP-006 mutation checks at {git('rev-parse', '--short', 'HEAD').strip()}")
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
        path = REPO / file
        original = path.read_text()
        if any(original.count(part) != 1 for part in as_list(old)):
            say(f"NO-VERDICT (pattern not found exactly once) {name}")
            nov += 1
            continue
        try:
            mutated = original
            for part, replacement in zip(as_list(old), as_list(new)):
                mutated = mutated.replace(part, replacement, 1)
            path.write_text(mutated)
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
