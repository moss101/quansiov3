# ADP-006 reconciliation

Output generators: slides rendering and export with a completion gate, diagrams, PDF, speech-to-text, video, web-dev
preview. Coverage: `PARTIAL`. CAP-003 gave the artifact store, kind and media-type checks, versions and `format_check`;
CAP-004 the Artifact tools; ADP-002 to ADP-005 the worker, the managed browser and the desktop shell. There was no deck
state, no quality record, no render sandbox, no PPTX/PDF/SVG writer or reader on the authority side, no video timeline, no
transcription route and no preview server. (Recorded when the task started; updated at the end with what was and was not
done, see NOTES.md.)

## Canonical owner

`Product` (task `owner`); paths from the task: `crates/server/src/artifacts/`, `python/intelligence/artifacts/`,
`apps/desktop/`.

- Authority (Rust): `crates/server/src/artifacts/outputs/` (new module inside the owner path). It verifies what the
  intelligence plane built and commits a version with a quality record the authority wrote itself. Hosts: `deck_host`,
  `docs_host`, `video_host`, `transcribe_host`, `preview_host`; checkers: `html`, `xml`, `ooxml`, `pdf`, `svg`;
  models: `deck`, `document`, `transcript`, `video/{timeline,ops,plan}`; `preview/` (scopes, authority port, loopback
  server, suggestions); `ports` (render), `media` (ffmpeg jobs), `transcription` (gateway route).
- Intelligence plane (Python): `python/intelligence/artifacts/outputs/` (slide templates, the OOXML writer, Markdown
  and diagram building) and `python/intelligence/model_gateway/transcription.py` plus the `transcribe` method of the one
  gateway. Python builds bytes and proposals; it commits nothing.
- Worker (Rust): `crates/qworkerd/src/browser/render.rs` (isolated rendering) and `crates/qworkerd/src/media.rs`
  (supervised media jobs). Outside the task's literal paths because the worker is where a browser and `ffmpeg` run; the
  server never depends on it in its own code (only its tests do).
- Desktop (TypeScript): `apps/desktop/src/artifacts/outputs.ts`, `renderer/OutputPanels.tsx` and the existing artifact
  browser. A projection only.

## Existing code and its coverage

| Code | Classification | What it did | What changes |
|---|---|---|---|
| `artifacts/formats.rs` | covered, extended | per-kind source checks; presentation parsing | presentation parsing delegates to `outputs::deck::parse_presentation` (layout, alt text, image, chart keys); `outputs::shapes` checks the new media types |
| `artifacts/tool_host.rs` (`ServerToolHost`) | covered, extended | routes artifact tools | routes the 13 output tools to `OutputToolHost`; `artifact.create`/`update` refuse tool-owned roles and media types |
| `artifacts/metadata.rs` | covered, extended | rows for artifacts and versions | `quality_record` on versions; `find_by_role`, `load_version`, `version_by_seq` |
| `api/artifacts.rs` | covered, extended | version listing | returns `quality_record` |
| `runtime/turn_loop/dispatch.rs` | covered, extended | host owner mapping | `OUTPUT_HOST_OWNER = "ADP-006"` |
| `python/intelligence/artifacts/formats.py` | covered, extended | the same source rules in Python | Slide gains `layout`, `alt`, `image`, `chart`; both validators agree on the shared corpus |
| `python/intelligence/model_gateway/gateway.py` | covered, extended | chat and embedding calls through one gateway | `transcribe` and `transcription_routes`; the data-policy guard runs before a byte is built |
| `qworkerd/browser/*` (CDP client, session) | covered, reused | managed browser | `render.rs` adds a fresh browser context per render with a request interceptor and no script |
| `config/models.yaml`, `config/tools.yaml` | covered, extended | catalog; tool declarations | `openai-whisper` (capability `transcription`); 13 `host: server` tools |

## Persistent state touched

- Postgres: one column, `artifact_versions.quality_record JSONB` (migration `0027`, nullable, no default, the table's
  existing row-level security applies). No new table, no new store, no job table.
- Object store: output bytes through the existing `ArtifactStore` (versions of Artifacts), screenshots through the
  existing `EvidenceStore`.
- Memory only (must not survive a restart): preview scopes and their tombstones; transcription jobs in the gateway
  process (`TranscriptionJobs`; the authority's stored transcripts are what make a request id idempotent across restarts).
- Target: media job directories under a managed root (`status.json`, `done.json`, logs) are the supervisor's bookkeeping
  inside its root, not a store; a render plan is typed data, the job id is its digest.

## Contracts and events changed (DOMAIN.md, same commits as the code)

§0 (terms), §7.5 (13 tool names), §10.1 (`quality_record`; optional slide keys), new normative §10.4 A to G (quality
record, decks and the completion gate, printing and diagrams and the render sandbox, video, speech-to-text, web-dev
preview, errors and effects), §12 rule 8 (a transcript is data), §15 (`Output` family: `DECK_NOT_DELIVERABLE` 409,
`ADAPTER_UNAVAILABLE` 503, `PREVIEW_SCOPE_LAPSED` 403). `config/outputs.yaml` with `schemas/json/outputs-config.schema.json`.
No new event type, no command.

## External effects and effect classes

Every output Tool but `deck.deliver` and `webdev.preview_suggestions` settles as `content.upload` (tier 1, idempotent by
digest): the version it adds. Those two are `read.internal`; the preview's file reads are `read.internal` and not
journaled. Transcription sends audio to a provider through the gateway (`content.upload` to the route), after the
data-policy guard. Nothing is retried blind: a transcription job is idempotent by `request_id`, a render job is
idempotent by the digest of its plan, a failed job is reported once.

## Capabilities and approvals

Capability only narrows. The tools carry `grant_templates` for `content.upload`/`read.internal` over artifacts or the
workspace file tree. The preview adds no capability: it serves what the run's own execution target can already read,
inside one directory, and lapses with the run or the member.

## Migrations

`0027_artifact_quality_record.sql`: `ALTER TABLE artifact_versions ADD COLUMN quality_record JSONB`. Forward-safe (no
default, nullable, no rewrite). Exercised by every server test that migrates a scratch database.

## Real boundaries and what they need

| Boundary | Environment | Host status |
|---|---|---|
| Managed browser layout, screenshots, print-to-PDF | Google Chrome | available: exercised for real (`outputs_deck`, `outputs_documents`, qworkerd `render`) |
| `ffmpeg` / `ffprobe` | `QUANSIO_TEST_FFMPEG_BIN` (ffprobe is looked for beside it) | **absent**: `outputs_video_ffmpeg` reports `BLOCKED_EXTERNAL`; supervision is tested with real processes and a stand-in toolchain that is marked as not ffmpeg |
| Speech-to-text provider | `QUANSIO_TEST_STT_API_KEY`, `QUANSIO_TEST_STT_AUDIO`, `QUANSIO_TEST_STT_EXPECT` (optional `QUANSIO_TEST_STT_BASE_URL`) | **absent**: `test_transcription_live.py` reports `BLOCKED_EXTERNAL`; the wire, the route, the data-policy guard and the job registry are tested against a real loopback HTTP provider |
| Loopback preview server, real HTTP, real files, real Postgres | none | exercised for real (`outputs_preview`) |

## Rollback and recovery impact

The column is additive. Outputs are versions of Artifacts: a bad one is superseded by the next version, nothing is
overwritten. Media jobs recover through `status.json`/`done.json` after a restart (attach by job id) and are killed as a
process group on cancel or timeout. Preview scopes and transcription jobs are volatile by design: after a restart a
scope's URL answers `NOT_FOUND` and a lost transcription is submitted again (it reads audio and changes nothing outside
the workspace).
