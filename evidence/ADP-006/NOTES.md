# ADP-006 notes

Output generators: deck rendering with a completion gate, PPTX/PDF export, Markdown to PDF, diagrams, video edit/render/
inspect, speech-to-text, web-dev preview, and the desktop's projection of their quality records. Status on this branch:
`BLOCKED_EXTERNAL` with `implementation_complete: true` (see the blocker in `registries/progress.json` after the merge).

## What ran for real, and what did not

Ran against real boundaries (logs in `2026-10-07T18-00-00Z/`):

- **A real browser.** Google Chrome, driven over CDP by qworkerd, lays out every deck page and document, measures it with
  a trusted program (overflow, emptiness, contrast, alt text, source text, network), screenshots it and prints the PDF
  (`outputs_deck` 5, `outputs_documents` 5, qworkerd `render` 10). The PPTX is checked by a reader written separately from
  the writer (Rust) and again in Python; macOS Quick Look drew the generated deck (title, content, quote, chart, image
  layouts) by eye. PowerPoint itself was not available.
- **Real Postgres, real object store, real dispatcher.** Every server suite drives the governed tool path (dispatch, effect
  ledger, artifact store) against a private database and the development MinIO.
- **Real sockets and files for the preview.** `outputs_preview` (12): real loopback HTTP requests (including hand-written
  request lines so `..` and escapes are sent as written), a real directory with real links, real authority changes in the
  database, a manual clock for exact expiry. `outputs_compose` (1): the deployable starts the listener.
- **Real processes for media supervision.** qworkerd `media` (12): process groups, signals, attach, restart recovery, with
  a **stand-in** `ffmpeg`/`ffprobe` script that is marked in its first lines as not ffmpeg. It proves supervision and
  composition; it proves nothing about what ffmpeg makes.
- **A real HTTP provider on loopback** for the transcription wire, route, data-policy guard and job registry
  (`test_transcription_outputs.py`, 34 tests). It is a stub of the provider, not the provider.
- The server's `audio.transcribe` host tests (`outputs_transcribe`, 7) use a **logic-level double of the gateway** behind
  the `TranscriptionPort`; they judge the authority's side only.

Did **not** run on this machine (explicit `BLOCKED_EXTERNAL`, never a pass):

| Suite | Variables |
|---|---|
| `crates/server/tests/outputs_video_ffmpeg.rs`: a real `ffmpeg` renders and `ffprobe` reads back | `QUANSIO_TEST_FFMPEG_BIN` (ffmpeg with libx264 and aac; ffprobe beside it) |
| `python/tests/intelligence/test_transcription_live.py`: a real provider transcribes a real recording | `QUANSIO_TEST_STT_API_KEY`, `QUANSIO_TEST_STT_AUDIO`, `QUANSIO_TEST_STT_EXPECT`, optional `QUANSIO_TEST_STT_BASE_URL` |

Unverified until the ffmpeg suite runs (written from the documentation, never executed): the filter graph's repeated
`[0:v:0]` uses, `amix normalize=0` (needs ffmpeg 4.4 or newer), the subtitle burn-in filter, the probe fields read back.

## Deferrals (none shrinks the task's acceptance)

- **No gRPC method carries `audio.transcribe` from Rust to the Python plane.** The Rust side is the `TranscriptionPort`
  and the Python side is `TranscriptionJobs` over the gateway; the seam between them is not generated. In the deployable
  the tool therefore fails `ADAPTER_UNAVAILABLE` (`details {adapter: "transcription"}`) until a transport is composed.
  Adding one means a proto method, a Python servicer, a Rust client and regenerated bindings (the Rust contract generator
  cannot run from a worktree).
- **Worker ports are not composed in the deployable**: the render host, the `ffmpeg` target and the transcription route.
  Those tools fail closed and name the adapter. The preview **is** composed (loopback listener started by the composition
  root when an object store is configured).
- **The preview is scope, authority, expiry, staging and a static file server.** An in-iframe edit runtime (a script
  injected into served pages that finds elements and sends suggestions) and a proxy to a running dev server are out of the
  contract (DOMAIN.md section 10.4 F) and are not built.
- The desktop shows decks, transcripts and timelines as text and counts; it never renders a page fragment or SVG as markup
  (APP-008's rule). A visual page preview would need a sandboxed frame and a policy change, and was not attempted.
- Windows and Linux paths for the render sandbox and the media supervisor (process groups) were not tried.

## Traps worth keeping

- **A positional test name filters every selected target.** `cargo test --lib outputs --test outputs_video` ran zero
  integration tests and every mutant "survived". Run the unit suite and one integration binary as separate commands.
- **`tempfile::tempdir()` makes `.tmpXXXX` directories.** A rule that refuses dot-prefixed path components then also
  refuses everything under the test's root and masks the guard being tested. Use a builder with a prefix.
- **Two layers hide each other.** The pages artifact is looked up by origin `run` and then must carry a quality record
  by `deck.render`; a forged upload was stopped by the first layer, so a test of the second needed a forged run-origin
  artifact. Mutate every layer, then each one alone.
- **Chrome PDFs are not byte-deterministic** (creation and modification dates); `pdf::canonicalise` rewrites the 14 date
  digits in place so a replay is the same version. Chrome writes text with two-byte Identity-H fonts, so a test reading
  text back needs the ToUnicode map.
- **`set_config` tenant context is transaction-local.** All store access in `outputs/support.rs` and the preview authority
  runs inside `pool.begin()` with the context set inside it.
- **arch_check "second scheduler".** A `tokio::spawn` followed by `loop {` is refused; the CDP interceptor uses
  `while let` over a `next_event` helper.
- **Composition gate.** A production type that implements a port but is never constructed (`DbPreviewAuthority`) fails
  `test_every_authority_is_composed_or_a_named_owned_gap`; composing it was the fix. A model identifier in a Python test
  file fails `hardcoded-model-id`.
- **Rust contract generation cannot run from a worktree.** `test_generated_bindings_are_current` is deselected here and
  must be run in the finalize dry-run clone; catalog, OpenAPI, TypeScript and SDK generation are checked in place.
- The stand-in `ffmpeg` writes the argument list and frame list into the sheet it makes, so two different inspections
  never return the same bytes (the replay detection would otherwise answer a new request with an old record).

## Mutation checks

`2026-10-07T18-00-00Z/mutation-checks.py` breaks 77 guards (Rust server, worker, Python, desktop), each by one edit; a
mutant counts as killed only if it compiles, its toolchain's unmutated suite passed first, and a named test then fails.
Result: 77 mutants, every one compiled, every control passed, 77 killed, 0 survived (76 in the full run, one more for the
composition root in a second run appended to the same log). The first full run found 16 survivors; each was a real hole
or a misassigned suite, and the tests were strengthened (a forged run-origin artifact with no record or another
adapter's record; an edited page that is not re-rendered stays edited in the stored pages; a reserved role with an
ordinary media type; another workspace's audio; the host's own batch bound; hidden directories that exist; the file size
bound; links that leave the directory; children of a cancelled job; a hostile diagram title).
One mutant was removed on purpose: `all_ready` in the answer of `deck.render` is a second layer over the gate, which
recomputes every slide from the stored source, so removing it changes nothing a test can see.

## Known limits

- Preview scope tombstones are remembered up to 4096; a lapsed scope older than that answers `NOT_FOUND` (not the
  lapse), which is the safe direction.
- The suggestions artifact gets a version per suggestion (at most `preview.max_suggestions`, 50).
- Transcription jobs are held in the gateway process; a restart forgets open ones and the caller's next poll submits
  again (the authority's stored transcript is what makes a finished request id idempotent).
